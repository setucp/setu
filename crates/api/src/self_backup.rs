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
pub const DUMP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// `.partial` files older than this are leftovers of a crashed dump.
const PARTIAL_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);
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
    Ok(db
        .settings()
        .get(setu_db::SELF_BACKUP)
        .await?
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

async fn store(db: &Db, rec: &SelfBackupRecord) -> Result<(), AppError> {
    let value = serde_json::to_value(rec).map_err(|_| AppError::internal())?;
    db.settings().set(setu_db::SELF_BACKUP, &value).await
}

/// One backup at `now`; returns (and stores) the updated record. A dump
/// failure is recorded, not returned: only database errors are `Err`.
pub async fn run_once(
    db: &Db,
    runner: Arc<dyn Runner + Send + Sync>,
    database_url: &str,
    data_dir: &Path,
    now: DateTime<Utc>,
) -> Result<SelfBackupRecord, AppError> {
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

/// Tick every `cfg.tick`; run a backup when [`is_due`].
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
        loop {
            every.tick().await;
            let now = clock.now();
            match load(&db).await {
                Ok(rec) if is_due(now, &rec, &cfg) => {
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
                        Err(e) => tracing::error!(error = %e, "self-backup record failed"),
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "self-backup state unreadable"),
            }
        }
    })
}
