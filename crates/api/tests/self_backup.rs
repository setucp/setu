//! Control-plane self-backup (0.1.1 #9, R145).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate (R146)

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{FixedOffset, TimeZone, Utc};
use setu_api::cli::{Invocation, Runner};
use setu_api::self_backup::{self, SelfBackupRecord, KEEP};
use setu_jobs::SchedulerConfig;
use setu_testkit::TestDb;

/// Writes 1 KiB where `pg_dump -f <out>` would, or fails.
struct FakeDump {
    fail: bool,
    seen: Mutex<Vec<Invocation>>,
}

impl Runner for FakeDump {
    fn run(&self, inv: &Invocation) -> Result<(), String> {
        self.seen.lock().unwrap().push(inv.clone());
        if self.fail {
            return Err("pg_dump failed: exit status: 1".into());
        }
        let out = inv.args.last().unwrap();
        std::fs::write(out, vec![0u8; 1024]).map_err(|e| e.to_string())
    }
}

/// A URL the `url` crate parses (an empty host does not parse).
const DB_URL: &str = "postgres://setu:Zq7vPassWordNeverOnArgv@127.0.0.1:5/setu";

fn cfg() -> SchedulerConfig {
    SchedulerConfig {
        utc_offset: Some(FixedOffset::east_opt(0).unwrap()),
        jitter_min: 0,
        ..SchedulerConfig::default()
    }
}

fn dumps(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn self_backup_writes_a_dump_keeps_seven_and_records_success() {
    let tdb = TestDb::new().await;
    let db = setu_db::Db::new(tdb.pool.clone());
    let data = tempfile::tempdir().unwrap();
    let dir = self_backup::backup_dir(data.path());
    std::fs::create_dir_all(&dir).unwrap();
    for day in 1..=8 {
        std::fs::write(
            dir.join(format!("setu-202610{day:02}T030000Z.dump")),
            b"old",
        )
        .unwrap();
    }
    let now = Utc.with_ymd_and_hms(2026, 10, 10, 3, 1, 0).unwrap();
    let runner = Arc::new(FakeDump {
        fail: false,
        seen: Mutex::new(vec![]),
    });

    let rec = self_backup::run_once(
        &db,
        runner.clone(),
        "postgres://setu:Zq7vPassWordNeverOnArgv@127.0.0.1:1/setu",
        data.path(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(rec.last_success_at, Some(now));
    assert_eq!(rec.file.as_deref(), Some("setu-20261010T030100Z.dump"));
    assert_eq!(rec.size_bytes, Some(1024));
    assert_eq!(rec.last_error, None);
    let left = dumps(&dir);
    assert_eq!(left.len(), KEEP, "{left:?}");
    assert_eq!(
        left.last().map(String::as_str),
        Some("setu-20261010T030100Z.dump")
    );
    assert!(!left.iter().any(|f| f.ends_with(".partial")));
    assert!(
        runner.seen.lock().unwrap()[0]
            .args
            .iter()
            .all(|a| !a.contains("Zq7vPassWordNeverOnArgv")),
        "password never on argv"
    );
    assert_eq!(
        self_backup::load(&db).await.unwrap(),
        rec,
        "stored in settings"
    );
}

#[tokio::test]
async fn self_backup_failure_keeps_the_last_success() {
    let tdb = TestDb::new().await;
    let db = setu_db::Db::new(tdb.pool.clone());
    let data = tempfile::tempdir().unwrap();
    let t1 = Utc.with_ymd_and_hms(2026, 10, 9, 3, 0, 0).unwrap();
    let ok = Arc::new(FakeDump {
        fail: false,
        seen: Mutex::new(vec![]),
    });
    self_backup::run_once(&db, ok, DB_URL, data.path(), t1)
        .await
        .unwrap();
    let t2 = t1 + chrono::Duration::days(1);
    let bad = Arc::new(FakeDump {
        fail: true,
        seen: Mutex::new(vec![]),
    });
    let rec = self_backup::run_once(&db, bad, DB_URL, data.path(), t2)
        .await
        .unwrap();
    assert_eq!(rec.last_success_at, Some(t1));
    assert_eq!(rec.last_attempt_at, Some(t2));
    assert_eq!(
        rec.last_error.as_deref(),
        Some("pg_dump failed: exit status: 1")
    );
    assert_eq!(
        dumps(&self_backup::backup_dir(data.path())).len(),
        1,
        "no partial left behind"
    );
}

#[test]
fn self_backup_is_due_once_per_night_and_retries_hourly() {
    let at = |d, h, m| Utc.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap();
    let none = SelfBackupRecord::default();
    assert!(
        !self_backup::is_due(at(10, 2, 59), &none, &cfg()),
        "before 03:00"
    );
    assert!(self_backup::is_due(at(10, 3, 0), &none, &cfg()));
    assert!(
        self_backup::is_due(at(10, 14, 0), &none, &cfg()),
        "no window: late is still due"
    );
    let done = SelfBackupRecord {
        last_success_at: Some(at(10, 3, 1)),
        ..none.clone()
    };
    assert!(
        !self_backup::is_due(at(10, 23, 0), &done, &cfg()),
        "once per night"
    );
    assert!(
        self_backup::is_due(at(11, 3, 0), &done, &cfg()),
        "next night"
    );
    let failed = SelfBackupRecord {
        last_attempt_at: Some(at(11, 3, 0)),
        ..done.clone()
    };
    assert!(
        !self_backup::is_due(at(11, 3, 30), &failed, &cfg()),
        "retry at most hourly"
    );
    assert!(self_backup::is_due(at(11, 4, 1), &failed, &cfg()));
}

#[tokio::test]
async fn panel_backup_endpoint_reports_the_last_success() {
    let tdb = TestDb::new().await;
    let srv = setu_api::testing::TestServer::spawn(tdb, setu_jobs::testing::FakeAgent::new()).await;
    let viewer = srv.login_as("viewer").await;
    let at = Utc.with_ymd_and_hms(2026, 10, 10, 3, 1, 0).unwrap();
    let rec = SelfBackupRecord {
        last_success_at: Some(at),
        file: Some("setu-20261010T030100Z.dump".into()),
        size_bytes: Some(1024),
        ..Default::default()
    };
    srv.db
        .settings()
        .set(setu_db::SELF_BACKUP, &serde_json::to_value(&rec).unwrap())
        .await
        .unwrap();
    let body: serde_json::Value = viewer
        .get(srv.url("/api/v1/panel/backup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["file"], "setu-20261010T030100Z.dump");
    assert_eq!(body["size_bytes"], 1024);
    assert!(body["dir"].as_str().unwrap().ends_with("backups/setu"));
}

/// M2: the dump holds the whole database, so its file is 0600 from the
/// moment it exists (the partial is created 0600 before `pg_dump` writes).
#[tokio::test]
async fn dump_file_is_0600() {
    use std::os::unix::fs::PermissionsExt as _;
    let tdb = TestDb::new().await;
    let db = setu_db::Db::new(tdb.pool.clone());
    let data = tempfile::tempdir().unwrap();
    let runner = Arc::new(FakeDump {
        fail: false,
        seen: Mutex::new(vec![]),
    });
    let now = Utc.with_ymd_and_hms(2026, 10, 10, 3, 1, 0).unwrap();
    let rec = self_backup::run_once(&db, runner, DB_URL, data.path(), now)
        .await
        .unwrap();
    let file = rec.file.expect("a dump was written");
    let path = self_backup::backup_dir(data.path()).join(file);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{}", path.display());
}

/// M2: a `.partial` left by a crashed dump is pruned once it is over an
/// hour old; a fresh one (maybe a dump in progress) is kept.
#[tokio::test]
async fn partial_files_are_pruned() {
    let tdb = TestDb::new().await;
    let db = setu_db::Db::new(tdb.pool.clone());
    let data = tempfile::tempdir().unwrap();
    let dir = self_backup::backup_dir(data.path());
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("setu-20261009T030000Z.dump.partial");
    let fresh = dir.join("setu-20261010T025900Z.dump.partial");
    std::fs::write(&old, b"half").unwrap();
    std::fs::write(&fresh, b"half").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_hours(2))
        .unwrap();
    let runner = Arc::new(FakeDump {
        fail: false,
        seen: Mutex::new(vec![]),
    });
    let now = Utc.with_ymd_and_hms(2026, 10, 10, 3, 1, 0).unwrap();
    self_backup::run_once(&db, runner, DB_URL, data.path(), now)
        .await
        .unwrap();
    let left = dumps(&dir);
    assert!(!left.contains(&"setu-20261009T030000Z.dump.partial".to_string()));
    assert!(left.contains(&"setu-20261010T025900Z.dump.partial".to_string()));
}
