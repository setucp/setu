//! Nightly backup of the control plane's own database (0.1.1 #9, R145):
//! `pg_dump -Fc` (via [`crate::cli::pg_dump_invocation`]: the password in
//! the child's environment, never argv) into `<data_dir>/backups/setu/`,
//! the newest [`KEEP`] kept, the outcome stored in setting `self_backup`
//! for Settings → Panel. Due once per night at the scheduler's backup
//! hour (jittered), as soon as that time has passed; failures retry at
//! most hourly.
//!
//! The dump is created 0600 (it holds the whole database) and a dump runs
//! at most [`DUMP_TIMEOUT`] before it is killed and recorded as a failure.
//! `.partial` files older than an hour are pruned with the old dumps.
//!
//! Unsupported: two control planes sharing one data dir. Each would prune
//! the other's dumps and race on the `.partial` names.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use setu_db::Db;
use setu_jobs::{Clock, SchedulerConfig};

use crate::cli::{pg_dump_invocation, Runner, TimedProcessRunner};

/// Dumps kept in [`backup_dir`].
pub const KEEP: usize = 7;
/// Longest a `pg_dump` may run before it is killed (M2).
pub const DUMP_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(30);
/// `.partial` files older than this are leftovers of a crashed dump.
const PARTIAL_MAX_AGE: std::time::Duration = std::time::Duration::from_hours(1);
/// Jitter key (the scheduler's `due_at` takes a site id).
const JITTER_KEY: &str = "setu-control-plane";

/// The stored outcome (setting `self_backup`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfBackupRecord {
    pub last_success_at: Option<DateTime<Utc>>,
    pub file: Option<String>,
    pub size_bytes: Option<i64>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// `<data_dir>/backups/setu`.
pub fn backup_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("backups").join("setu")
}

/// Due when today's time has passed, nothing succeeded since 12 h
/// before it, and no attempt in the last hour.
pub fn is_due(now: DateTime<Utc>, rec: &SelfBackupRecord, cfg: &SchedulerConfig) -> bool {
    let due = setu_jobs::scheduler::due_at(now, JITTER_KEY, cfg);
    if now < due {
        return false;
    }
    if rec
        .last_success_at
        .is_some_and(|t| t >= due - chrono::Duration::hours(12))
    {
        return false;
    }
    rec.last_attempt_at
        .is_none_or(|t| now - t >= chrono::Duration::hours(1))
}

/// The stored record; missing or unreadable is the default.
pub async fn load(db: &Db) -> Result<SelfBackupRecord, AppError> {
    load_raw(db).await.map_err(|e| {
        tracing::error!(error = %e, "self-backup state unreadable");
        AppError::internal()
    })
}

/// [`load`] with the database error itself, so the loop can name the
/// cause instead of logging a bare "internal error" (#14).
async fn load_raw(db: &Db) -> Result<SelfBackupRecord, sqlx::Error> {
    Ok(db
        .settings()
        .try_get(setu_db::SELF_BACKUP)
        .await?
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

/// Holds the exclusive backup lock (`<data_dir>/backups/setu/.lock`) until
/// dropped. It is an `flock` on a file, so it serialises the scheduler's
/// dump, `setu backup` and the upgrade's pre-dump whether they run in
/// one process or in two (#15).
#[derive(Debug)]
pub struct BackupLock {
    _file: std::fs::File,
}

impl BackupLock {
    /// The lock, or `conflict` ("another backup is running") when
    /// another dump holds it.
    pub fn try_acquire(data_dir: &Path) -> Result<Self, AppError> {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let dir = backup_dir(data_dir);
        std::fs::create_dir_all(&dir).map_err(|e| {
            tracing::error!(error = %e, dir = %dir.display(), "cannot create the backup directory");
            AppError::internal()
        })?;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(dir.join(".lock"))
            .map_err(|e| {
                tracing::error!(error = %e, "cannot open the backup lock");
                AppError::internal()
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err(AppError::conflict("another backup is running"))
            }
            Err(std::fs::TryLockError::Error(e)) => {
                tracing::error!(error = %e, "cannot take the backup lock");
                Err(AppError::internal())
            }
        }
    }
}

async fn store(db: &Db, rec: &SelfBackupRecord) -> Result<(), AppError> {
    let value = serde_json::to_value(rec).map_err(|_| AppError::internal())?;
    db.settings().set(setu_db::SELF_BACKUP, &value).await
}

/// One backup at `now`; returns (and stores) the updated record. A dump
/// failure is recorded, not returned: only database errors and a held
/// [`BackupLock`] (`conflict`) are `Err`.
pub async fn run_once(
    db: &Db,
    runner: Arc<dyn Runner + Send + Sync>,
    database_url: &str,
    data_dir: &Path,
    now: DateTime<Utc>,
) -> Result<SelfBackupRecord, AppError> {
    // Before the load: a busy lock changes nothing (no attempt is
    // recorded), the caller retries on its next tick.
    let _lock = BackupLock::try_acquire(data_dir)?;
    let mut rec = load(db).await?;
    rec.last_attempt_at = Some(now);
    let dir = backup_dir(data_dir);
    let name = format!("setu-{}.dump", now.format("%Y%m%dT%H%M%SZ"));
    let partial = dir.join(format!("{name}.partial"));
    let finished = dir.join(&name);
    let outcome = dump(runner, database_url, &dir, &partial, &finished).await;
    match outcome {
        Ok(size) => {
            rec.last_success_at = Some(now);
            rec.file = Some(name);
            rec.size_bytes = Some(size);
            rec.last_error = None;
            prune(&dir);
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            tracing::error!(error = %e, "control-plane self-backup failed");
            rec.last_error = Some(e);
        }
    }
    store(db, &rec).await?;
    Ok(rec)
}

async fn dump(
    runner: Arc<dyn Runner + Send + Sync>,
    database_url: &str,
    dir: &Path,
    partial: &Path,
    finished: &Path,
) -> Result<i64, String> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
    let inv = pg_dump_invocation(database_url, partial)?;
    // Created 0600 before pg_dump writes into it: pg_dump keeps the mode
    // of an existing file. A leftover of the same name is replaced.
    let _ = std::fs::remove_file(partial);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(partial)
        .map_err(|e| format!("cannot create the dump file: {e}"))?;
    // The runner kills pg_dump at DUMP_TIMEOUT; this backstop (a minute
    // later) only guards a runner that does not.
    let backstop = DUMP_TIMEOUT + std::time::Duration::from_secs(60);
    tokio::time::timeout(
        backstop,
        tokio::task::spawn_blocking(move || runner.run(&inv)),
    )
    .await
    .map_err(|_| format!("pg_dump did not finish within {} s", DUMP_TIMEOUT.as_secs()))?
    .map_err(|e| format!("pg_dump task: {e}"))??;
    std::fs::rename(partial, finished).map_err(|e| format!("cannot finish the dump: {e}"))?;
    let len = std::fs::metadata(finished)
        .map_err(|e| e.to_string())?
        .len();
    Ok(i64::try_from(len).unwrap_or(i64::MAX))
}

/// Keep the newest [`KEEP`] `setu-*.dump` files (names sort by time), and
/// remove `setu-*.dump.partial` files over [`PARTIAL_MAX_AGE`] old.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut dumps: Vec<PathBuf> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("setu-") {
            continue;
        }
        if name.ends_with(".dump.partial") {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age >= PARTIAL_MAX_AGE);
            if old {
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(error = %e, file = %path.display(), "pruning a partial dump failed");
                }
            }
        } else if Path::new(name).extension().is_some_and(|e| e == "dump") {
            dumps.push(path);
        }
    }
    dumps.sort();
    let excess = dumps.len().saturating_sub(KEEP);
    for old in dumps.into_iter().take(excess) {
        if let Err(e) = std::fs::remove_file(&old) {
            tracing::warn!(error = %e, file = %old.display(), "pruning a self-backup failed");
        }
    }
}

/// Consecutive unreadable-state ticks before the single warning (#14).
pub const WARN_AFTER: u32 = 5;

/// Failures of the state read, across ticks.
#[derive(Debug, Default)]
struct Outage {
    failures: u32,
    since: Option<DateTime<Utc>>,
    cause: String,
}

impl Outage {
    /// Count a failure; true exactly once, at [`WARN_AFTER`].
    fn fail(&mut self, now: DateTime<Utc>, cause: String) -> bool {
        self.failures += 1;
        self.since.get_or_insert(now);
        self.cause = cause;
        self.failures == WARN_AFTER
    }

    /// The read works again: the note for `last_error` when the outage
    /// was long enough to have cost a backup, and the counters reset.
    fn recover(&mut self, now: DateTime<Utc>) -> Option<String> {
        let note = (self.failures >= WARN_AFTER).then(|| {
            format!(
                "the database was unreadable from {} to {} ({} attempts, no backup ran): {}",
                self.since.unwrap_or(now).format("%Y-%m-%d %H:%M UTC"),
                now.format("%Y-%m-%d %H:%M UTC"),
                self.failures,
                self.cause
            )
        });
        *self = Self::default();
        note
    }

    /// Ticks to skip after this many failures: 0, 1, 3, 7, capped at 9
    /// (about ten minutes at the scheduler's tick), so a dead database
    /// is not asked every minute.
    fn skip(&self) -> u32 {
        match self.failures {
            0 | 1 => 0,
            n => ((1u32 << (n - 1).min(4)) - 1).min(9),
        }
    }
}

/// Tick every `cfg.tick`; run a backup when [`is_due`]. A failing state
/// read is logged with its cause, retried with backoff, warned about
/// once, and recorded in `last_error` when it clears.
pub fn spawn(
    db: Db,
    clock: Arc<dyn Clock>,
    database_url: String,
    data_dir: PathBuf,
    cfg: SchedulerConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(cfg.tick);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut outage = Outage::default();
        let mut skipping = 0u32;
        loop {
            every.tick().await;
            if skipping > 0 {
                skipping -= 1;
                continue;
            }
            let now = clock.now();
            let mut rec = match load_raw(&db).await {
                Ok(rec) => rec,
                Err(e) => {
                    tracing::error!(error = %e, failures = outage.failures + 1, "self-backup state unreadable");
                    if outage.fail(now, e.to_string()) {
                        tracing::warn!(
                            error = %e,
                            "self-backup: the database has been unreadable for {WARN_AFTER} attempts; no backup is running"
                        );
                    }
                    skipping = outage.skip();
                    continue;
                }
            };
            if let Some(note) = outage.recover(now) {
                tracing::warn!("self-backup: {note}");
                rec.last_error = Some(note);
                if let Err(e) = store(&db, &rec).await {
                    tracing::error!(error = %e, "self-backup outage note not stored");
                }
            }
            if !is_due(now, &rec, &cfg) {
                continue;
            }
            match run_once(
                &db,
                Arc::new(TimedProcessRunner {
                    timeout: DUMP_TIMEOUT,
                }),
                &database_url,
                &data_dir,
                now,
            )
            .await
            {
                Ok(r) if r.last_error.is_none() => {
                    tracing::info!(file = ?r.file, "control-plane self-backup written");
                }
                Ok(_) => {}
                Err(e) if e.code == setu_db::ErrorCode::Conflict => {
                    tracing::info!("self-backup waits: {}", e.message);
                }
                Err(e) => tracing::error!(error = %e, "self-backup record failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(m: u32) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + i64::from(m) * 60, 0).unwrap()
    }

    #[test]
    fn warns_once_and_notes_a_long_outage_on_recovery() {
        let mut o = Outage::default();
        let warned: Vec<bool> = (0..7).map(|i| o.fail(t(i), format!("boom {i}"))).collect();
        assert_eq!(warned, [false, false, false, false, true, false, false]);
        let note = o.recover(t(30)).expect("long outage is noted");
        assert!(
            note.contains("7 attempts") && note.contains("boom 6"),
            "{note}"
        );
        assert_eq!(o.failures, 0);
        assert!(o.recover(t(31)).is_none());
    }

    #[test]
    fn a_short_blip_leaves_no_note() {
        let mut o = Outage::default();
        o.fail(t(0), "blip".into());
        o.fail(t(1), "blip".into());
        assert!(o.recover(t(2)).is_none());
    }

    #[test]
    fn backoff_grows_then_caps() {
        let mut o = Outage::default();
        let mut skips = Vec::new();
        for i in 0..8 {
            o.fail(t(i), String::new());
            skips.push(o.skip());
        }
        assert_eq!(skips, [0, 1, 3, 7, 9, 9, 9, 9]);
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = BackupLock::try_acquire(dir.path()).unwrap();
        let err = BackupLock::try_acquire(dir.path()).unwrap_err();
        assert_eq!(err.message, "another backup is running");
        drop(first);
        BackupLock::try_acquire(dir.path()).expect("free again");
    }
}
