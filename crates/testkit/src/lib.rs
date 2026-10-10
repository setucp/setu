// Test double / test harness: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Test helpers: throwaway Postgres databases, one per test.

use sqlx::postgres::PgPoolOptions;
use sqlx::AssertSqlSafe;
use sqlx::PgPool;

/// Database names are generated as `setu_test_<ulid>`: lowercase
/// alphanumeric plus underscores, safe to interpolate as an identifier.
fn assert_generated_db_name(db_name: &str) {
    assert!(is_generated_db_name(db_name), "unexpected database name");
}

/// Connection URL of the throwaway Postgres maintenance database.
///
/// Honors `SETU_TEST_PG_URL`; otherwise builds
/// `postgres://setu:setu@127.0.0.1:${SETU_TEST_PG_PORT:-55440}/postgres`.
/// Panics on port 5432: tests must never touch production Postgres.
pub fn pg_url() -> String {
    let url = match std::env::var("SETU_TEST_PG_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            let port = std::env::var("SETU_TEST_PG_PORT").unwrap_or_else(|_| "55440".to_owned());
            format!("postgres://setu:setu@127.0.0.1:{port}/postgres")
        }
    };
    assert!(
        explicit_port(&url) != Some(5432),
        "refusing to run tests against port 5432"
    );
    url
}

/// Explicit port from a Postgres URL's authority section, if present.
///
/// Parses host/port structurally instead of substring-matching so that
/// `:5432` is rejected regardless of trailing path, query, or fragment,
/// while userinfo and database names containing "5432" do not trip the
/// guard. Returns `None` when no explicit port is present or the URL is
/// too malformed to parse (callers treat that as "no port found").
fn explicit_port(url: &str) -> Option<u16> {
    let after_scheme = url.split("://").nth(1)?;
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .filter(|s| !s.is_empty())?;
    // Strip userinfo (`user:pass@`) if present.
    let host_port = authority.rsplit('@').next()?;
    let port_str = if let Some((_, after)) = host_port.rsplit_once(']') {
        // Possibly a bracketed IPv6 literal: `[::1]:5432`. Anything that
        // is not exactly `[...]:port` has no parseable explicit port.
        after.strip_prefix(':').filter(|s| !s.is_empty())?
    } else {
        host_port.rfind(':').map(|i| &host_port[i + 1..])?
    };
    if port_str.is_empty() {
        return None;
    }
    port_str.parse::<u16>().ok()
}

fn db_url_for(maintenance_url: &str, db_name: &str) -> String {
    let (base, _) = maintenance_url
        .rsplit_once('/')
        .expect("pg url must contain a database name");
    format!("{base}/{db_name}")
}

/// True when `db_name` is a name this module generated itself
/// (`setu_test_<ulid>`): lowercase alphanumeric plus underscores.
fn is_generated_db_name(db_name: &str) -> bool {
    db_name.starts_with("setu_test_")
        && !db_name["setu_test_".len()..].is_empty()
        && db_name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

async fn drop_database(maintenance_url: &str, db_name: &str) {
    // Never panic on the cleanup path: Drop must not unwind.
    if !is_generated_db_name(db_name) {
        return;
    }
    let Ok(admin) = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(maintenance_url)
        .await
    else {
        return;
    };
    // Best-effort: ignore every error. WITH (FORCE) disconnects our own
    // pool's lingering sessions so the drop succeeds at test end.
    let sql = AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#
    ));
    let _ = sqlx::query(sql).execute(&admin).await;
    admin.close().await;
}

/// One throwaway database on the test Postgres container.
///
/// Created as `setu_test_<ulid>` (unique across parallel tests), migrated
/// with `crates/db/migrations`, dropped on `Drop` (best-effort, synchronous
/// with a timeout; never panics).
pub struct TestDb {
    pub pool: PgPool,
    pub url: String,
    db_name: String,
    /// Maintenance-DB URL captured at construction. Stored so `Drop` never
    /// re-reads the environment (which may have changed or be gone).
    maintenance_url: String,
}

impl TestDb {
    pub async fn new() -> Self {
        let maintenance_url = pg_url();
        let db_name = format!("setu_test_{}", ulid::Ulid::new().to_string().to_lowercase());
        assert_generated_db_name(&db_name);
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&maintenance_url)
            .await
            .expect("connect to test Postgres maintenance database");
        let sql = AssertSqlSafe(format!(r#"CREATE DATABASE "{db_name}""#));
        sqlx::query(sql)
            .execute(&admin)
            .await
            .expect("create throwaway test database");
        admin.close().await;
        let url = db_url_for(&maintenance_url, &db_name);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .expect("connect to throwaway test database");
        sqlx::migrate!("../db/migrations")
            .run(&pool)
            .await
            .expect("run test database migrations");
        Self {
            pool,
            url,
            db_name,
            maintenance_url,
        }
    }
}

/// How long `Drop` waits for the helper thread's `DROP DATABASE` before
/// giving up (detaching) rather than stalling test teardown forever.
const DROP_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

impl Drop for TestDb {
    fn drop(&mut self) {
        // Never re-read env here: use the URL captured at construction.
        // Never panic: every failure mode below is swallowed.
        let maintenance_url = self.maintenance_url.clone();
        let db_name = self.db_name.clone();
        // Run the async cleanup on a helper thread with its own
        // current-thread runtime, so there is no `block_on` nested inside
        // a surrounding tokio runtime (safe in the common `#[tokio::test]`
        // case). The join below makes the drop actually complete before
        // `Drop` returns instead of leaking the database.
        let worker = std::thread::spawn(move || {
            let _ = std::panic::catch_unwind(|| {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    rt.block_on(drop_database(&maintenance_url, &db_name));
                }
            });
        });
        let deadline = std::time::Instant::now() + DROP_JOIN_TIMEOUT;
        loop {
            if worker.is_finished() {
                let _ = worker.join();
                return;
            }
            if std::time::Instant::now() >= deadline {
                // Timed out: detach the worker (dropping the handle) and
                // return rather than stalling teardown forever.
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn two_dbs_are_isolated() {
        let a = TestDb::new().await;
        let b = TestDb::new().await;
        sqlx::query("CREATE TABLE probe (id INT)")
            .execute(&a.pool)
            .await
            .expect("create probe table in db a");
        sqlx::query("INSERT INTO probe (id) VALUES (1)")
            .execute(&a.pool)
            .await
            .expect("insert into probe in db a");
        let seen: Result<(i64,), sqlx::Error> = sqlx::query_as("SELECT COUNT(*) FROM probe")
            .fetch_one(&b.pool)
            .await;
        // Either outcome proves isolation: zero rows, or (expected) the
        // table does not exist at all in db b. Any other error is a real
        // failure and must fail the test, not pass vacuously.
        match seen {
            Ok((n,)) => assert_eq!(n, 0, "db b must not see rows from db a"),
            Err(sqlx::Error::Database(db_err)) if db_err.code().as_deref() == Some("42P01") => {
                // Undefined table in db b: isolated, as expected.
            }
            Err(e) => panic!("unexpected error querying db b: {e}"),
        }
    }
}
