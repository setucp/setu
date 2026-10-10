//! Postgres admin adapter (spec §4 Postgres): the [`PostgresAdmin`]
//! trait, [`PsqlSudo`] over a [`CommandRunner`] (peer auth as `postgres`
//! through `sudo -n`), and an in-memory [`FakePostgres`].
//!
//! Passwords never reach argv: psql reads a script on stdin that starts
//! with `\set pw '<value>'` and uses `:'pw'`. Errors carry one line of
//! stderr with the password replaced by `***`. Dumps go to stdout
//! (`pg_dump -Fc <db>`), streamed into a file under the agent's own
//! `/var/lib/setu/backups`; `pg_restore` reads one streamed from a file
//! (I3: a dump never sits in agent memory); `restore_replace` drops the
//! database first so objects a failed version created do not survive.
//! Every database and role name must be `site_<short>` (M11).

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Postgres surface the agent needs. Every call is idempotent.
#[async_trait::async_trait]
pub trait PostgresAdmin: Send + Sync {
    /// Create the role (LOGIN) and database (owned by it) when absent,
    /// set the role's password, revoke `CONNECT` from `PUBLIC`.
    async fn create_or_alter(&self, db: &str, role: &str, password: &str) -> anyhow::Result<()>;
    /// Drop the database (forcing connections off) and the role.
    async fn drop(&self, db: &str, role: &str) -> anyhow::Result<()>;
    /// `pg_dump -Fc` of the database, streamed into the file `to`
    /// (created 0600); returns its size in bytes.
    async fn dump(&self, db: &str, to: &Path) -> anyhow::Result<u64>;
    /// Replace the database with the custom-format dump in the file
    /// `from`: drop with force, create again (role password untouched),
    /// `pg_restore --no-owner --role=<role>` with the file streamed to
    /// its stdin.
    async fn restore_replace(&self, db: &str, role: &str, from: &Path) -> anyhow::Result<()>;
    /// Databases named `site_*` (for `Inventory`).
    async fn list_databases(&self) -> anyhow::Result<Vec<String>>;
}

/// What a finished process returned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs one program with bytes on stdin and captures its output.
#[async_trait::async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, argv: &[String], stdin: &[u8]) -> anyhow::Result<RunOutput>;
    /// Run with stdin streamed from the file `stdin_from` (else empty)
    /// and stdout streamed into the file `stdout_to` (then `stdout` of
    /// the result is empty), never buffering either.
    async fn run_io(
        &self,
        argv: &[String],
        stdin_from: Option<&Path>,
        stdout_to: Option<&Path>,
    ) -> anyhow::Result<RunOutput>;
}

/// Production runner: `tokio::process`, environment inherited (sudo
/// resets it), stdin written then closed.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioRunner;

#[async_trait::async_trait]
impl CommandRunner for TokioRunner {
    async fn run(&self, argv: &[String], stdin: &[u8]) -> anyhow::Result<RunOutput> {
        let out = crate::stream::run(argv, stdin.to_vec(), None, None).await?;
        Ok(RunOutput {
            success: out.success,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    async fn run_io(
        &self,
        argv: &[String],
        stdin_from: Option<&Path>,
        stdout_to: Option<&Path>,
    ) -> anyhow::Result<RunOutput> {
        let out = crate::stream::run(argv, Vec::new(), stdin_from, stdout_to).await?;
        Ok(RunOutput {
            success: out.success,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// One recorded call: argv and the bytes written to stdin.
pub type RecordedCall = (Vec<String>, Vec<u8>);

/// Recording runner for tests: every call's argv and stdin; replies
/// from a queue (default: success with empty output).
#[derive(Debug, Clone, Default)]
pub struct FakeRunner {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    replies: Arc<Mutex<VecDeque<RunOutput>>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue the reply for the next call.
    pub fn push(&self, reply: RunOutput) {
        self.replies.lock().expect("fake runner").push_back(reply);
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("fake runner").clone()
    }
}

#[async_trait::async_trait]
impl CommandRunner for FakeRunner {
    async fn run(&self, argv: &[String], stdin: &[u8]) -> anyhow::Result<RunOutput> {
        self.calls
            .lock()
            .expect("fake runner")
            .push((argv.to_vec(), stdin.to_vec()));
        Ok(self
            .replies
            .lock()
            .expect("fake runner")
            .pop_front()
            .unwrap_or(RunOutput {
                success: true,
                ..RunOutput::default()
            }))
    }

    /// Records the file's bytes as stdin; a reply's stdout goes into
    /// `stdout_to` (tests only: inputs are small).
    async fn run_io(
        &self,
        argv: &[String],
        stdin_from: Option<&Path>,
        stdout_to: Option<&Path>,
    ) -> anyhow::Result<RunOutput> {
        let stdin = match stdin_from {
            Some(path) => std::fs::read(path)?,
            None => Vec::new(),
        };
        let mut out = self.run(argv, &stdin).await?;
        if let Some(path) = stdout_to {
            std::fs::write(path, &out.stdout)?;
            out.stdout = Vec::new();
        }
        Ok(out)
    }
}

/// Database and role names are exactly `site_<short>`
/// (`^site_[a-z0-9]{8}$`): the agent never touches `postgres`,
/// `template*` or anything a site does not own (M11).
fn check_ident(name: &str, what: &str) -> anyhow::Result<()> {
    if !name
        .strip_prefix("site_")
        .is_some_and(setu_common::is_short_id)
    {
        anyhow::bail!("invalid postgres {what} name: must be site_<short id>");
    }
    Ok(())
}

/// psql single-quoted meta-command argument: `'` doubles, `\` escapes.
/// Control characters (a newline would end the `\set` line) are refused.
fn psql_quote(value: &str) -> anyhow::Result<String> {
    if value.chars().any(char::is_control) {
        anyhow::bail!("postgres password contains a control character");
    }
    Ok(format!(
        "'{}'",
        value.replace('\\', "\\\\").replace('\'', "''")
    ))
}

/// `CREATE ROLE … LOGIN` when absent (`\gexec`: no DO block needed).
fn create_role_sql(role: &str) -> String {
    format!(
        "SELECT format('CREATE ROLE %I LOGIN', '{role}') \
         WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') \\gexec\n"
    )
}

/// `CREATE DATABASE … OWNER …` when absent (it cannot run inside a
/// transaction block, hence `\gexec`), then the PUBLIC revoke.
fn create_db_and_revoke_sql(db: &str, role: &str) -> String {
    format!(
        "SELECT format('CREATE DATABASE %I OWNER %I', '{db}', '{role}') \
         WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = '{db}') \\gexec\n\
         REVOKE CONNECT ON DATABASE \"{db}\" FROM PUBLIC;\n"
    )
}

/// Production [`PostgresAdmin`]: `sudo -n -u postgres psql -v
/// ON_ERROR_STOP=1 -X -q` with the script on stdin.
#[derive(Debug, Clone, Default)]
pub struct PsqlSudo<R: CommandRunner = TokioRunner> {
    runner: R,
}

impl PsqlSudo<TokioRunner> {
    pub fn system() -> Self {
        Self {
            runner: TokioRunner,
        }
    }
}

impl<R: CommandRunner> PsqlSudo<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    fn as_postgres(prog: &str, args: &[&str]) -> Vec<String> {
        ["sudo", "-n", "-u", "postgres", prog]
            .iter()
            .chain(args)
            .map(|s| s.to_string())
            .collect()
    }

    /// Run `prog` as postgres; a failure is one stderr line with every
    /// `secret` replaced by `***`.
    async fn exec(
        &self,
        prog: &str,
        args: &[&str],
        stdin: &[u8],
        secret: Option<&str>,
    ) -> anyhow::Result<Vec<u8>> {
        let out = self
            .runner
            .run(&Self::as_postgres(prog, args), stdin)
            .await?;
        if out.success {
            return Ok(out.stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut line = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no error output")
            .to_string();
        if let Some(secret) = secret.filter(|s| !s.is_empty()) {
            line = line.replace(secret, "***");
        }
        anyhow::bail!("{prog} failed: {line}")
    }

    /// [`Self::exec`] with stdin from / stdout into files (dumps).
    async fn exec_io(
        &self,
        prog: &str,
        args: &[&str],
        stdin_from: Option<&Path>,
        stdout_to: Option<&Path>,
    ) -> anyhow::Result<()> {
        let out = self
            .runner
            .run_io(&Self::as_postgres(prog, args), stdin_from, stdout_to)
            .await?;
        if out.success {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let line = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no error output");
        anyhow::bail!("{prog} failed: {line}")
    }

    async fn psql(&self, script: &str, secret: Option<&str>) -> anyhow::Result<()> {
        self.exec(
            "psql",
            &["-v", "ON_ERROR_STOP=1", "-X", "-q"],
            script.as_bytes(),
            secret,
        )
        .await
        .map(|_| ())
    }
}

#[async_trait::async_trait]
impl<R: CommandRunner> PostgresAdmin for PsqlSudo<R> {
    async fn create_or_alter(&self, db: &str, role: &str, password: &str) -> anyhow::Result<()> {
        check_ident(db, "database")?;
        check_ident(role, "role")?;
        let script = format!(
            "\\set pw {pw}\n{create_role}ALTER ROLE \"{role}\" WITH LOGIN PASSWORD :'pw';\n{rest}",
            pw = psql_quote(password)?,
            create_role = create_role_sql(role),
            rest = create_db_and_revoke_sql(db, role),
        );
        self.psql(&script, Some(password)).await
    }

    async fn drop(&self, db: &str, role: &str) -> anyhow::Result<()> {
        check_ident(db, "database")?;
        check_ident(role, "role")?;
        self.psql(
            &format!(
                "DROP DATABASE IF EXISTS \"{db}\" WITH (FORCE);\nDROP ROLE IF EXISTS \"{role}\";\n"
            ),
            None,
        )
        .await
    }

    async fn dump(&self, db: &str, to: &Path) -> anyhow::Result<u64> {
        check_ident(db, "database")?;
        self.exec_io("pg_dump", &["-Fc", db], None, Some(to))
            .await?;
        Ok(std::fs::metadata(to)?.len())
    }

    async fn restore_replace(&self, db: &str, role: &str, from: &Path) -> anyhow::Result<()> {
        check_ident(db, "database")?;
        check_ident(role, "role")?;
        self.psql(
            &format!("DROP DATABASE IF EXISTS \"{db}\" WITH (FORCE);\n"),
            None,
        )
        .await?;
        let ensure = create_role_sql(role) + &create_db_and_revoke_sql(db, role);
        self.psql(&ensure, None).await?;
        let role_arg = format!("--role={role}");
        self.exec_io(
            "pg_restore",
            &["--no-owner", &role_arg, "-d", db],
            Some(from),
            None,
        )
        .await
    }

    async fn list_databases(&self) -> anyhow::Result<Vec<String>> {
        let out = self
            .exec(
                "psql",
                &[
                    "-X",
                    "-q",
                    "-At",
                    "-c",
                    "SELECT datname FROM pg_database WHERE datname LIKE 'site\\_%' ORDER BY 1",
                ],
                &[],
                None,
            )
            .await?;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }
}

#[derive(Debug, Default)]
struct FakeInner {
    calls: Vec<String>,
    databases: HashMap<String, String>,
}

/// In-memory `PostgresAdmin` for runtime tests: `dump` returns a marker,
/// nothing touches a real server.
#[derive(Debug, Clone, Default)]
pub struct FakePostgres {
    inner: Arc<Mutex<FakeInner>>,
}

impl FakePostgres {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().expect("fake postgres lock")
    }

    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    pub fn has_db(&self, db: &str) -> bool {
        self.lock().databases.contains_key(db)
    }
}

#[async_trait::async_trait]
impl PostgresAdmin for FakePostgres {
    async fn create_or_alter(&self, db: &str, role: &str, _password: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("create {db} {role}"));
        inner.databases.insert(db.to_string(), role.to_string());
        Ok(())
    }

    async fn drop(&self, db: &str, role: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("drop {db} {role}"));
        inner.databases.remove(db);
        Ok(())
    }

    async fn dump(&self, db: &str, to: &Path) -> anyhow::Result<u64> {
        self.lock().calls.push(format!("dump {db}"));
        let bytes = format!("fake-dump-of:{db}\n").into_bytes();
        std::fs::write(to, &bytes)?;
        Ok(bytes.len() as u64)
    }

    async fn restore_replace(&self, db: &str, role: &str, from: &Path) -> anyhow::Result<()> {
        let dump = std::fs::read(from)?;
        let mut inner = self.lock();
        inner.calls.push(format!("restore {db} {role}"));
        if dump != format!("fake-dump-of:{db}\n").into_bytes() {
            anyhow::bail!("fake restore: not a dump of {db}");
        }
        inner.databases.insert(db.to_string(), role.to_string());
        Ok(())
    }

    async fn list_databases(&self) -> anyhow::Result<Vec<String>> {
        let mut dbs: Vec<String> = self.lock().databases.keys().cloned().collect();
        dbs.sort();
        Ok(dbs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_idents_pass() {
        check_ident("site_abc12345", "database").expect("valid");
    }

    #[test]
    fn hostile_idents_fail() {
        for bad in [
            "",
            "a-b",
            "a b",
            "a\"; DROP",
            "x'.y",
            "site_abc123",
            "postgres",
        ] {
            check_ident(bad, "database").expect_err("must reject");
        }
    }
}
