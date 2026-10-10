// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Delete and delete_backup over [`FakeAgent`] (plan Task 6).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    ctx_with_logs, exec, lease_to_test_worker, manifest_toml, setup, test_sealer, wait_job,
};
use setu_jobs::{
    backup, delete, delete_backup, provision, testing::FakeAgent, Runner, RunnerConfig, SystemClock,
};

#[tokio::test]
async fn delete_backs_up_then_removes_everything_and_the_row() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    let before = agent.transcript().len();
    logs.lock().unwrap().clear();

    delete::run(&ctx, &fx.site_id).await.expect("delete");

    assert_eq!(
        agent.transcript()[before..],
        [
            "backup",
            "remove_route",
            "stop",
            "drop_database",
            "site_remove"
        ]
        .map(str::to_string)
    );
    let drop = &agent.commands_of("drop_database")[0];
    assert_eq!(
        (drop["db"].as_str(), drop["role"].as_str()),
        (Some(site.db_name.as_str()), Some(site.db_role.as_str()))
    );
    assert_eq!(
        agent.commands_of("site_remove")[0]["site"],
        site.short_id.as_str()
    );
    assert!(fx.db.sites().get(&fx.site_id).await.is_err(), "row gone");

    // The pre_delete backup outlives the site with its name and domain.
    let backups = fx.db.backups().list_for_site(&fx.site_id).await.unwrap();
    assert!(backups.is_empty(), "site_id nulled");
    let id = agent.commands_of("backup")[0]["backup_id"]
        .as_str()
        .unwrap()
        .to_string();
    let row = fx.db.backups().get(&id).await.unwrap();
    assert_eq!(row.kind, "pre_delete");
    assert_eq!(row.status, "completed");
    assert_eq!(row.site_id, None);
    assert_eq!(
        (row.site_name.as_str(), row.domain.as_str()),
        ("hello-site", fx.domain.as_str())
    );
    assert_eq!(row.site_short_id, site.short_id);
    let expect: Vec<String> = delete::STEPS
        .iter()
        .map(|s| format!("step {s}: ok"))
        .collect();
    assert_eq!(*logs.lock().unwrap(), expect);
}

#[tokio::test]
async fn delete_aborts_when_the_pre_delete_backup_fails() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    agent.fail_next("backup");
    let before = agent.transcript().len();

    delete::run(&ctx, &fx.site_id)
        .await
        .expect_err("no delete without a backup");
    assert_eq!(agent.transcript()[before..], ["backup".to_string()]);
    assert_eq!(
        fx.db.sites().get(&fx.site_id).await.unwrap().status,
        "running"
    );
}

#[tokio::test]
async fn delete_backup_issues_command_and_marks_row() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    let id = backup::run(&ctx, &fx.site_id, "manual")
        .await
        .expect("backup");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    logs.lock().unwrap().clear();

    delete_backup::run(&ctx, &id).await.expect("delete backup");
    let cmds = agent.commands_of("delete_backup");
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0]["site"], site.short_id.as_str());
    assert_eq!(cmds[0]["backup_id"], id.as_str());
    assert_eq!(fx.db.backups().get(&id).await.unwrap().status, "deleted");
    assert_eq!(
        *logs.lock().unwrap(),
        [
            "step validate: ok",
            "step delete_backup: ok",
            "step finish: ok"
        ]
    );

    // Already deleted: no second command, still a success.
    delete_backup::run(&ctx, &id).await.expect("idempotent");
    assert_eq!(agent.commands_of("delete_backup").len(), 1);
}

fn fast() -> RunnerConfig {
    RunnerConfig {
        concurrency: 1,
        lease: Duration::from_secs(1),
        renew_every: Duration::from_millis(200),
        installs_per_server: 2,
        idle_poll: Duration::from_millis(50),
        verify_delay: Duration::ZERO,
    }
}

#[tokio::test]
async fn delete_job_through_the_runner_ends_done_and_keeps_its_log() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    let job = fx
        .db
        .jobs()
        .enqueue("delete_site", Some(&fx.site_id), Some(&fx.server_id), None)
        .await
        .unwrap();
    let runner = Runner::spawn(
        fx.db.clone(),
        Arc::new(agent.clone()),
        test_sealer(),
        Arc::new(SystemClock),
        fast(),
    );

    let done = wait_job(&fx.db, &job.id, Duration::from_secs(20)).await;
    runner.abort();
    assert_eq!(done.status, "done", "error: {:?}", done.error);
    assert_eq!(done.site_id, None, "SET NULL kept the row");
    assert!(done.log.contains("step finish: ok"), "{}", done.log);
    assert!(
        fx.db.sites().get(&fx.site_id).await.is_err(),
        "site row gone"
    );
}

/// Review Focus 3: a delete job reclaimed after `finish` removed the row
/// arrives with `site_id = NULL` and must end `done`.
#[tokio::test]
async fn reclaimed_delete_job_after_the_row_is_gone_ends_done() {
    let (test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let job = fx
        .db
        .jobs()
        .enqueue("delete_site", Some(&fx.site_id), Some(&fx.server_id), None)
        .await
        .unwrap();
    lease_to_test_worker(&fx.db, &job.id).await;
    exec(
        &test_db,
        "UPDATE jobs SET payload = jsonb_set(payload, '{step}', '\"finish\"') WHERE id = $1",
        &job.id,
    )
    .await;
    fx.db
        .sites()
        .delete(&fx.site_id)
        .await
        .expect("row removed by finish");
    exec(
        &test_db,
        "UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1",
        &job.id,
    )
    .await;
    let runner = Runner::spawn(
        fx.db.clone(),
        Arc::new(agent.clone()),
        test_sealer(),
        Arc::new(SystemClock),
        fast(),
    );

    let done = wait_job(&fx.db, &job.id, Duration::from_secs(20)).await;
    runner.abort();
    assert_eq!(done.status, "done", "error: {:?}", done.error);
    assert!(agent.transcript().is_empty(), "nothing re-run on the agent");
}
