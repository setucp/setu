//! Postgres admin adapter: a trait, the `sudo -u postgres psql` impl,
//! and an in-memory fake for tests.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Postgres surface the agent needs. `create_db_and_role` is idempotent
/// (safe to redeliver) and refreshes the role password every call so a
/// rotated password converges without a separate step.
#[async_trait::async_trait]
pub trait PostgresAdmin: Send + Sync {
    async fn create_db_and_role(&self, db: &str, role: &str, password: &str) -> anyhow::Result<()>;
    async fn drop_db_and_role(&self, db: &str, role: &str) -> anyhow::Result<()>;
    async fn dump(&self, db: &str, dest: &Path) -> anyhow::Result<()>;
    async fn restore(&self, db: &str, src: &Path) -> anyhow::Result<()>;
}

fn escape_literal(s: &str) -> String {
    s.replace('\'', "''")
}

fn check_ident(name: &str, what: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        anyhow::bail!("invalid postgres {what} name: {name}");
    }
    Ok(())
}

/// Production impl: `sudo -u postgres psql -v ON_ERROR_STOP=1`.
#[derive(Debug, Clone, Default)]
pub struct PsqlCli;

impl PsqlCli {
    pub fn new() -> Self {
        Self
    }

    async fn psql(&self, sql: &str) -> anyhow::Result<String> {
        let out = tokio::process::Command::new("sudo")
            // `-n`: never prompt (a missing rule fails, never hangs).
            .arg("-n")
            .arg("-u")
            .arg("postgres")
            .arg("psql")
            .arg("-v")
            .arg("ON_ERROR_STOP=1")
            .arg("-tA")
            .arg("-c")
            .arg(sql)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("cannot run sudo -u postgres psql: {e}"))?;
        if !out.status.success() {
            anyhow::bail!("psql failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    async fn run_as_postgres(&self, prog: &str, args: &[&str]) -> anyhow::Result<()> {
        let out = tokio::process::Command::new("sudo")
            // `-n`: never prompt (a missing rule fails, never hangs).
            .arg("-n")
            .arg("-u")
            .arg("postgres")
            .arg(prog)
            .args(args)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("cannot run sudo -u postgres {prog}: {e}"))?;
        if !out.status.success() {
            anyhow::bail!("{prog} failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl PostgresAdmin for PsqlCli {
    async fn create_db_and_role(&self, db: &str, role: &str, password: &str) -> anyhow::Result<()> {
        check_ident(db, "database")?;
        check_ident(role, "role")?;
        // Role: create when missing, always refresh the password.
        let role_sql = format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') \
             THEN CREATE ROLE \"{role}\" WITH LOGIN; END IF; END $$; \
             ALTER ROLE \"{role}\" WITH LOGIN PASSWORD '{pw}';",
            role = escape_literal(role),
            pw = escape_literal(password),
        );
        self.psql(&role_sql).await?;
        // Database: create when missing with the role as owner.
        let exists = self
            .psql(&format!(
                "SELECT 1 FROM pg_database WHERE datname = '{db}';",
                db = escape_literal(db)
            ))
            .await?;
        if !exists.lines().any(|l| l.trim() == "1") {
            self.psql(&format!(
                "CREATE DATABASE \"{db}\" OWNER \"{role}\";",
                db = db
            ))
            .await?;
        }
        Ok(())
    }

    async fn drop_db_and_role(&self, db: &str, role: &str) -> anyhow::Result<()> {
        check_ident(db, "database")?;
        check_ident(role, "role")?;
        // Drop connections first so the drop does not hang on a live site.
        let _ = self
            .psql(&format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{db}' AND pid <> pg_backend_pid();",
                db = escape_literal(db)
            ))
            .await;
        self.psql(&format!("DROP DATABASE IF EXISTS \"{db}\";", db = db))
            .await?;
        self.psql(&format!("DROP ROLE IF EXISTS \"{role}\";", role = role))
            .await?;
        Ok(())
    }

    async fn dump(&self, db: &str, dest: &Path) -> anyhow::Result<()> {
        check_ident(db, "database")?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| anyhow::anyhow!("cannot create dump dir: {e}"))?;
        }
        self.run_as_postgres(
            "pg_dump",
            &[
                "-Fc",
                db,
                "-f",
                dest.to_str()
                    .ok_or_else(|| anyhow::anyhow!("non-utf8 dump path"))?,
            ],
        )
        .await
    }

    async fn restore(&self, db: &str, src: &Path) -> anyhow::Result<()> {
        check_ident(db, "database")?;
        self.run_as_postgres(
            "pg_restore",
            &[
                "-d",
                db,
                "-c",
                src.to_str()
                    .ok_or_else(|| anyhow::anyhow!("non-utf8 restore path"))?,
            ],
        )
        .await
    }
}

#[derive(Debug, Default)]
struct FakeInner {
    calls: Vec<String>,
    databases: HashMap<String, String>,
}

/// In-memory `PostgresAdmin` for tests. `dump` writes a marker file so a
/// later `restore` has something to read; nothing touches a real server.
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
    async fn create_db_and_role(
        &self,
        db: &str,
        role: &str,
        _password: &str,
    ) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("create {db} {role}"));
        inner.databases.insert(db.to_string(), role.to_string());
        Ok(())
    }

    async fn drop_db_and_role(&self, db: &str, role: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("drop {db} {role}"));
        inner.databases.remove(db);
        Ok(())
    }

    async fn dump(&self, db: &str, dest: &Path) -> anyhow::Result<()> {
        self.lock().calls.push(format!("dump {db}"));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("fake dump mkdir: {e}"))?;
        }
        std::fs::write(dest, format!("fake-dump-of:{db}\n"))
            .map_err(|e| anyhow::anyhow!("fake dump write: {e}"))?;
        Ok(())
    }

    async fn restore(&self, db: &str, src: &Path) -> anyhow::Result<()> {
        self.lock().calls.push(format!("restore {db}"));
        std::fs::read(src).map_err(|e| anyhow::anyhow!("fake restore read: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_idents_pass() {
        check_ident("site_abc123", "database").expect("valid");
    }

    #[test]
    fn hostile_idents_fail() {
        for bad in ["", "a-b", "a b", "a\"; DROP", "x'.y"] {
            check_ident(bad, "database").expect_err("must reject");
        }
    }
}
