//! Scheduler tests on a [`FixedClock`]: one nightly backup per site per
//! day, and `delete_backup` jobs for copies past retention.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use common::{exec, manifest_toml, setup, test_sealer, wait_job};
use setu_jobs::testing::{FakeAgent, FixedClock};
use setu_jobs::{Runner, RunnerConfig, Scheduler, SchedulerConfig};

fn config() -> SchedulerConfig {
    SchedulerConfig {
        utc_offset: Some(chrono::FixedOffset::east_opt(0).unwrap()),
        ..SchedulerConfig::default()
    }
}

#[tokio::test]
async fn scheduler_enqueues_nightly_backup_once() {
    let (test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    fx.db
        .sites()
        .set_status(&fx.site_id, "running", None)
        .await
        .unwrap();
    let clock = FixedClock::new(Utc.with_ymd_and_hms(2026, 10, 9, 2, 0, 0).unwrap());
    let cfg = config();

    // 02:00 is before 03:00 - 20 min: nothing due.
    let r = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
    assert!(r.backups.is_empty(), "{r:?}");

    clock.set(Utc.with_ymd_and_hms(2026, 10, 9, 3, 30, 0).unwrap());
    let r = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
    assert_eq!(r.backups, std::slice::from_ref(&fx.site_id));
    let job = fx
        .db
        .jobs()
        .active_for_site(&fx.site_id)
        .await
        .unwrap()
        .expect("job");
    common::lease_to_test_worker(&fx.db, &job.id).await;
    assert_eq!(job.kind, "backup_site");
    assert_eq!(job.payload["kind"], "scheduled");

    // Queued, then taken: never a second one the same day, also after
    // a control-plane restart (the state is in the database).
    let r = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
    assert!(r.backups.is_empty(), "{r:?}");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    let row = fx.db.backups().create(&site, "scheduled").await.unwrap();
    exec(
        &test_db,
        "UPDATE backups SET taken_at = '2026-10-09T03:31:00Z' WHERE id = $1",
        &row.id,
    )
    .await;
    common::finish_job(&fx.db, &job.id, true).await;
    clock.set(Utc.with_ymd_and_hms(2026, 10, 9, 4, 0, 0).unwrap());
    let r = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
    assert!(r.backups.is_empty(), "{r:?}");

    // The next night it is due again.
    clock.set(Utc.with_ymd_and_hms(2026, 10, 10, 3, 30, 0).unwrap());
    let r = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
    assert_eq!(r.backups, std::slice::from_ref(&fx.site_id));
}

#[tokio::test]
async fn scheduler_enqueues_delete_backup_beyond_seven() {
    let (test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    let mut scheduled = Vec::new();
    for day in 1..=9 {
        let row = fx.db.backups().create(&site, "scheduled").await.unwrap();
        let at = format!("2026-10-{day:02}T03:00:00Z");
        exec(
            &test_db,
            &format!("UPDATE backups SET taken_at = '{at}' WHERE id = $1"),
            &row.id,
        )
        .await;
        scheduled.push(row.id);
    }
    let mut kinds = Vec::new();
    for (kind, at) in [
        ("pre_update", "2026-09-01T00:00:00Z"),
        ("pre_delete", "2026-10-01T00:00:00Z"),
        ("manual", "2026-01-01T00:00:00Z"),
    ] {
        let row = fx.db.backups().create(&site, kind).await.unwrap();
        exec(
            &test_db,
            &format!("UPDATE backups SET taken_at = '{at}' WHERE id = $1"),
            &row.id,
        )
        .await;
        kinds.push(row.id);
    }
    let clock = FixedClock::new(Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0).unwrap());
    let cfg = config();

    let mut want = vec![scheduled[0].clone(), scheduled[1].clone(), kinds[0].clone()];
    want.sort();
    // A live site's delete_backup is a job on the site (R136): one per
    // tick, the next once the site's active job finished.
    let r = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
    assert_eq!(r.deletes.len(), 1, "{r:?}");
    assert!(want.contains(&r.deletes[0]), "{r:?}");
    let again = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
    assert!(again.deletes.is_empty(), "site busy: {again:?}");
    let mut got = r.deletes.clone();

    // The runner executes them: one DeleteBackup each, rows deleted.
    let agent = FakeAgent::new();
    let runner = Runner::spawn(
        fx.db.clone(),
        Arc::new(agent.clone()),
        test_sealer(),
        Arc::new(clock.clone()),
        RunnerConfig {
            idle_poll: Duration::from_millis(50),
            ..RunnerConfig::default()
        },
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        for id in &got {
            if let Some(job) = fx.db.jobs().active_delete_backup(id).await.unwrap() {
                let done = wait_job(&fx.db, &job.id, Duration::from_secs(10)).await;
                assert_eq!(done.status, "done", "error: {:?}", done.error);
            }
        }
        if got.len() == want.len() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{got:?}");
        let r = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
        got.extend(r.deletes);
    }
    got.sort();
    assert_eq!(
        got, want,
        "two oldest scheduled + the 41-day-old pre_update"
    );
    for id in &want {
        let mut status = String::new();
        for _ in 0..100 {
            status = fx.db.backups().get(id).await.unwrap().status;
            if status == "deleted" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(status, "deleted", "{id}");
    }
    runner.abort();
    assert_eq!(agent.call_count("delete_backup"), 3);
    let after = Scheduler::tick(&fx.db, &clock, &cfg).await.unwrap();
    assert!(after.deletes.is_empty(), "{after:?}");
    let left = fx.db.backups().list_for_site(&fx.site_id).await.unwrap();
    assert_eq!(
        left.len(),
        7 + 1 + 1,
        "seven scheduled, recent pre_delete, manual"
    );
}
