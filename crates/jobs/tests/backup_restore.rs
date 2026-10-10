// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Backup and restore over [`FakeAgent`] (plan Task 6).

mod common;

use common::{ctx_with_logs, job_ctx, manifest_toml, setup};
use setu_jobs::{backup, provision, restore, testing::FakeAgent};

#[tokio::test]
async fn backup_uses_60_minute_ttl() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");

    let id = backup::run(&ctx, &fx.site_id, "manual")
        .await
        .expect("backup");
    let row = fx.db.backups().get(&id).await.unwrap();
    assert_eq!(
        (row.kind.as_str(), row.status.as_str(), row.size_bytes),
        ("manual", "completed", Some(4096))
    );
    assert_eq!(agent.commands_of("backup")[0]["backup_id"], id.as_str());

    restore::run(&ctx, &fx.site_id, &id).await.expect("restore");
    let ttls: Vec<(String, u64)> = agent
        .ttls()
        .into_iter()
        .filter(|(k, _)| k == "backup" || k == "restore")
        .map(|(k, d)| (k, d.as_secs()))
        .collect();
    assert_eq!(
        ttls,
        [
            ("backup".to_string(), 3600),
            ("backup".to_string(), 3600),
            ("restore".to_string(), 3600)
        ]
    );

    // A failed agent backup leaves a `failed` row behind.
    agent.fail_next("backup");
    backup::run(&ctx, &fx.site_id, "manual")
        .await
        .expect_err("agent failure");
    let rows = fx.db.backups().list_for_site(&fx.site_id).await.unwrap();
    assert!(rows.iter().any(|b| b.status == "failed"));
}

#[tokio::test]
async fn restore_takes_pre_restore_backup_stops_restores_starts() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    let source = backup::run(&ctx, &fx.site_id, "manual")
        .await
        .expect("backup");
    let before = agent.transcript().len();

    let (ctx, job_id, logs) = job_ctx(
        &fx.db,
        &agent,
        "restore_site",
        &fx.site_id,
        serde_json::json!({"backup_id": source}),
    )
    .await;
    restore::run(&ctx, &fx.site_id, &source)
        .await
        .expect("restore");

    assert_eq!(
        agent.transcript()[before..],
        ["backup", "stop", "restore", "start", "health"].map(str::to_string)
    );
    let rows = fx.db.backups().list_for_site(&fx.site_id).await.unwrap();
    let pre = rows
        .iter()
        .find(|b| b.kind == "pre_restore")
        .expect("pre_restore backup");
    assert_eq!(pre.status, "completed");
    assert_eq!(agent.commands_of("backup")[1]["backup_id"], pre.id.as_str());
    let cmd = &agent.commands_of("restore")[0];
    assert_eq!(cmd["backup_id"], source.as_str());
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(cmd["role"], site.db_role.as_str());
    assert_eq!(cmd["dirs"], serde_json::json!(["media"]));
    assert_eq!(site.status, "running");
    let expect: Vec<String> = restore::STEPS
        .iter()
        .map(|s| format!("step {s}: ok"))
        .collect();
    assert_eq!(*logs.lock().unwrap(), expect);
    let job = fx.db.jobs().get(&job_id).await.unwrap();
    assert_eq!(job.payload["pre_backup_id"], pre.id.as_str());

    // A failing restore marks the site failed at that step.
    agent.fail_next("restore");
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    restore::run(&ctx, &fx.site_id, &source)
        .await
        .expect_err("restore fails");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(
        (site.status.as_str(), site.failed_step.as_deref()),
        ("failed", Some("restore"))
    );
}
