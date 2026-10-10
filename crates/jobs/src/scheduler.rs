//! Scheduler (spec §6 "Backup"): every minute it enqueues one nightly
//! `backup_site` (kind `scheduled`) per running site at `backup_hour`
//! local time ± a per-site jitter, `delete_backup` jobs for scheduled
//! copies beyond the newest 7 and for `pre_*` copies older than 30 days,
//! and prunes metrics (30 d), finished agent commands (24 h) and expired
//! sessions.
//!
//! Idempotent across restarts: a site is due once per local day, and is
//! skipped when its newest `scheduled` backup was taken at or after that
//! day's due time − 12 h (so a backup taken just before the jittered due
//! time still counts) or when it already has an active job; a backup gets
//! a `delete_backup` job only when none is active for it, on its live
//! site (a busy site's waits for a later tick, R136).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveTime, Offset, TimeZone, Utc};
use setu_common::AppError;
use setu_db::Db;

use crate::Clock;

/// Scheduler tuning; [`Default`] is spec §6.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Local hour of the nightly backup.
    pub backup_hour: u32,
    /// Per-site jitter, ± minutes around `backup_hour:00`.
    pub jitter_min: u32,
    /// A site whose due time passed less than this long ago is still
    /// due (covers a control-plane restart around 03:00).
    pub catch_up: Duration,
    /// Scheduled copies kept per site.
    pub keep_scheduled: i64,
    /// Age after which `pre_*` copies are deleted.
    pub pre_keep: Duration,
    pub metrics_keep: Duration,
    pub commands_keep: Duration,
    /// The zone "local" means; `None` = the control plane's.
    pub utc_offset: Option<FixedOffset>,
    pub tick: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            backup_hour: 3,
            jitter_min: 20,
            catch_up: Duration::from_hours(2),
            keep_scheduled: 7,
            pre_keep: Duration::from_hours(720),
            metrics_keep: Duration::from_hours(720),
            commands_keep: Duration::from_hours(24),
            utc_offset: None,
            tick: Duration::from_secs(60),
        }
    }
}

/// What one tick enqueued (tests and logs).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// Site ids that got a nightly `backup_site`.
    pub backups: Vec<String>,
    /// Backup ids that got a `delete_backup`.
    pub deletes: Vec<String>,
}

/// Background scheduler.
pub struct Scheduler;

impl Scheduler {
    /// Tick now, then every `config.tick`, until aborted.
    pub fn spawn(
        db: Db,
        clock: Arc<dyn Clock>,
        config: SchedulerConfig,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut every = tokio::time::interval(config.tick);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                match Self::tick(&db, clock.as_ref(), &config).await {
                    Ok(r) if r.backups.is_empty() && r.deletes.is_empty() => {}
                    Ok(r) => tracing::info!(
                        backups = r.backups.len(),
                        deletes = r.deletes.len(),
                        "scheduler enqueued jobs"
                    ),
                    Err(e) => tracing::error!(error = %e, "scheduler tick failed"),
                }
            }
        })
    }

    /// One pass: enqueue what is due, then prune.
    pub async fn tick(
        db: &Db,
        clock: &dyn Clock,
        config: &SchedulerConfig,
    ) -> Result<TickReport, AppError> {
        let now = clock.now();
        let mut report = TickReport::default();
        let sites = db.sites().list().await?;

        for site in sites.iter().filter(|s| s.status == "running") {
            let due = due_at(now, &site.id, config);
            let window = chrono::Duration::from_std(config.catch_up).unwrap_or_default();
            if now < due || now >= due + window {
                continue;
            }
            if let Some(last) = db.backups().latest_scheduled_at(&site.id).await? {
                if last >= due - chrono::Duration::hours(12) {
                    continue; // today's is taken (or was attempted)
                }
            }
            if db.jobs().active_for_site(&site.id).await?.is_some() {
                continue; // busy; the next tick tries again
            }
            let payload = serde_json::json!({ "kind": "scheduled" });
            match db
                .jobs()
                .enqueue(
                    "backup_site",
                    Some(&site.id),
                    Some(&site.server_id),
                    Some(&payload),
                )
                .await
            {
                Ok(_) => report.backups.push(site.id.clone()),
                Err(e) if e.code == setu_common::ErrorCode::Conflict => {}
                Err(e) => return Err(e),
            }
        }

        let mut expired = Vec::new();
        for site in &sites {
            expired.extend(
                db.backups()
                    .scheduled_beyond(&site.id, config.keep_scheduled)
                    .await?,
            );
        }
        let pre_cutoff = now - chrono::Duration::from_std(config.pre_keep).unwrap_or_default();
        expired.extend(db.backups().pre_taken_before(pre_cutoff).await?);
        for backup in expired {
            if db.jobs().active_delete_backup(&backup.id).await?.is_some() {
                continue;
            }
            // On the site while it exists, so it serialises with the
            // site's jobs (an update or restore may read this backup); a
            // busy site is retried next tick. Server-level once the site
            // is gone (R89, R136).
            let payload = serde_json::json!({ "backup_id": backup.id });
            match db
                .jobs()
                .enqueue(
                    "delete_backup",
                    backup.site_id.as_deref(),
                    Some(&backup.server_id),
                    Some(&payload),
                )
                .await
            {
                Ok(_) => report.deletes.push(backup.id),
                Err(e) if e.code == setu_common::ErrorCode::Conflict => {}
                Err(e) => return Err(e),
            }
        }

        db.metrics().prune(config.metrics_keep).await?;
        db.commands().prune(config.commands_keep).await?;
        db.sessions().purge_expired().await?;
        Ok(report)
    }
}

/// Today's (local) due time of `site_id`'s nightly backup:
/// `backup_hour:00` plus a jitter in `[-jitter_min, +jitter_min]`
/// minutes derived from the site id (stable across restarts).
#[allow(clippy::cast_sign_loss)] // reviewed: kept as-is (R150)
pub fn due_at(now: DateTime<Utc>, site_id: &str, config: &SchedulerConfig) -> DateTime<Utc> {
    let offset = config.utc_offset.unwrap_or_else(|| {
        chrono::Local
            .offset_from_utc_datetime(&now.naive_utc())
            .fix()
    });
    let local_day = now.with_timezone(&offset).date_naive();
    let at = NaiveTime::from_hms_opt(config.backup_hour.min(23), 0, 0).unwrap_or_default();
    let base = offset
        .from_local_datetime(&local_day.and_time(at))
        .single()
        .map_or(now, |d| d.with_timezone(&Utc));
    let span = i64::from(config.jitter_min) * 2 + 1;
    let hash = site_id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let jitter = i64::try_from(hash % span as u64).unwrap_or(0) - i64::from(config.jitter_min);
    base + chrono::Duration::minutes(jitter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_time_is_stable_and_within_jitter() {
        let cfg = SchedulerConfig {
            utc_offset: Some(FixedOffset::east_opt(0).unwrap()),
            ..SchedulerConfig::default()
        };
        let now = Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap();
        let three = Utc.with_ymd_and_hms(2026, 10, 9, 3, 0, 0).unwrap();
        let mut seen = std::collections::HashSet::new();
        for i in 0..200 {
            let id = format!("site-{i}");
            let due = due_at(now, &id, &cfg);
            assert_eq!(due, due_at(now, &id, &cfg));
            assert!((due - three).num_minutes().abs() <= 20, "{due}");
            seen.insert(due);
        }
        assert!(seen.len() > 10, "jitter spreads sites");
        let ist = SchedulerConfig {
            utc_offset: Some(FixedOffset::east_opt(5 * 3600 + 1800).unwrap()),
            jitter_min: 0,
            ..SchedulerConfig::default()
        };
        // 03:00 IST is 21:30 UTC the day before.
        assert_eq!(
            due_at(now, "x", &ist),
            Utc.with_ymd_and_hms(2026, 10, 8, 21, 30, 0).unwrap()
        );
    }
}
