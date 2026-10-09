//! `PsqlSudo` over a recording `FakeRunner` (plan Task 10c): exact argv,
//! password only on stdin, redacted errors, REVOKE, and the replacing
//! restore order.

use setu_agent::postgres::{FakeRunner, PostgresAdmin, PsqlSudo, RunOutput};

const PSQL: [&str; 9] = [
    "sudo",
    "-n",
    "-u",
    "postgres",
    "psql",
    "-v",
    "ON_ERROR_STOP=1",
    "-X",
    "-q",
];
const PW: &str = "Zx9secretPw42";

fn argv(call: &(Vec<String>, Vec<u8>)) -> Vec<&str> {
    call.0.iter().map(String::as_str).collect()
}

fn stdin(call: &(Vec<String>, Vec<u8>)) -> String {
    String::from_utf8(call.1.clone()).expect("utf8 stdin")
}

#[tokio::test]
async fn create_or_alter_sends_password_on_stdin_only() {
    let runner = FakeRunner::new();
    let pg = PsqlSudo::new(runner.clone());
    pg.create_or_alter("site_abcd1234", "site_abcd1234", PW)
        .await
        .expect("create_or_alter");
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(argv(&calls[0]), PSQL.to_vec(), "argv is exact");
    assert!(
        calls.iter().all(|c| c.0.iter().all(|a| !a.contains(PW))),
        "password on argv"
    );
    let input = stdin(&calls[0]);
    assert!(
        input.starts_with(&format!("\\set pw '{PW}'\n")),
        "stdin begins with \\set pw: {}",
        input.lines().next().unwrap_or_default().len()
    );
    assert_eq!(input.matches(PW).count(), 1, "password appears once");
    for needle in [
        "CREATE ROLE %I LOGIN",
        "ALTER ROLE \"site_abcd1234\" WITH LOGIN PASSWORD :'pw';",
        "CREATE DATABASE %I OWNER %I",
    ] {
        assert!(input.contains(needle), "stdin lacks {needle}");
    }
    // Quotes are escaped defensively; control characters are refused.
    pg.create_or_alter("site_abcd1234", "site_abcd1234", "a'b\\c")
        .await
        .expect("quoted");
    assert!(stdin(&runner.calls()[1]).starts_with("\\set pw 'a''b\\\\c'\n"));
    pg.create_or_alter("site_abcd1234", "site_abcd1234", "a\nb")
        .await
        .expect_err("newline refused");
    pg.create_or_alter("site_x; DROP", "r", "pw")
        .await
        .expect_err("bad identifier refused");
    assert_eq!(runner.calls().len(), 2, "refused before running anything");
}

#[tokio::test]
async fn psql_errors_are_redacted() {
    let runner = FakeRunner::new();
    runner.push(RunOutput {
        success: false,
        stdout: Vec::new(),
        stderr: format!("psql:<stdin>:3: ERROR:  syntax error at or near \"{PW}\"\nmore {PW}\n")
            .into_bytes(),
    });
    let pg = PsqlSudo::new(runner.clone());
    let err = pg
        .create_or_alter("site_abcd1234", "site_abcd1234", PW)
        .await
        .expect_err("psql failed");
    let msg = format!("{err:#}");
    assert!(!msg.contains(PW), "password leaked in error");
    assert!(msg.contains("***"), "redaction marker missing: {msg}");
    assert!(!msg.contains('\n'), "one line only");
}

#[tokio::test]
async fn restore_replace_drops_with_force_then_restores() {
    let runner = FakeRunner::new();
    let pg = PsqlSudo::new(runner.clone());
    pg.restore_replace("site_abcd1234", "site_abcd1234", b"PGDMP-bytes".to_vec())
        .await
        .expect("restore_replace");
    let calls = runner.calls();
    assert_eq!(calls.len(), 3, "drop, create, pg_restore");
    assert_eq!(argv(&calls[0]), PSQL.to_vec());
    assert!(
        stdin(&calls[0]).contains("DROP DATABASE IF EXISTS \"site_abcd1234\" WITH (FORCE);"),
        "first the database is dropped with force"
    );
    assert_eq!(argv(&calls[1]), PSQL.to_vec());
    let create = stdin(&calls[1]);
    assert!(create.contains("CREATE DATABASE %I OWNER %I"));
    assert!(create.contains("REVOKE CONNECT ON DATABASE \"site_abcd1234\" FROM PUBLIC;"));
    assert!(
        !create.contains("\\set pw"),
        "restore keeps the role's password"
    );
    assert_eq!(
        argv(&calls[2]),
        vec![
            "sudo",
            "-n",
            "-u",
            "postgres",
            "pg_restore",
            "--no-owner",
            "--role=site_abcd1234",
            "-d",
            "site_abcd1234",
        ]
    );
    assert_eq!(
        calls[2].1, b"PGDMP-bytes",
        "the dump goes to pg_restore on stdin"
    );

    // dump: pg_dump -Fc to stdout, captured.
    runner.push(RunOutput {
        success: true,
        stdout: b"PGDMP-out".to_vec(),
        stderr: Vec::new(),
    });
    let bytes = pg.dump("site_abcd1234").await.expect("dump");
    assert_eq!(bytes, b"PGDMP-out");
    assert_eq!(
        argv(&runner.calls()[3]),
        vec![
            "sudo",
            "-n",
            "-u",
            "postgres",
            "pg_dump",
            "-Fc",
            "site_abcd1234"
        ]
    );
}

#[tokio::test]
async fn revoke_public_connect_is_issued() {
    let runner = FakeRunner::new();
    let pg = PsqlSudo::new(runner.clone());
    pg.create_or_alter("site_abcd1234", "site_abcd1234", PW)
        .await
        .expect("create_or_alter");
    let input = stdin(&runner.calls()[0]);
    let create = input.find("CREATE DATABASE").expect("create database");
    let revoke = input
        .find("REVOKE CONNECT ON DATABASE \"site_abcd1234\" FROM PUBLIC;")
        .expect("revoke issued");
    assert!(revoke > create, "revoke follows the create");

    pg.drop("site_abcd1234", "site_abcd1234")
        .await
        .expect("drop");
    let input = stdin(&runner.calls()[1]);
    assert!(input.contains("DROP DATABASE IF EXISTS \"site_abcd1234\" WITH (FORCE);"));
    assert!(input.contains("DROP ROLE IF EXISTS \"site_abcd1234\";"));
}
