//! Update state-machine tests over [`FakeAgent`].

mod common;

use common::{
    ctx_with_logs, manifest_toml, manifest_toml_command, manifest_toml_image, publish_version,
    setup,
};
use setu_jobs::{provision, testing::FakeAgent, update};

/// Provision 0.1.0 to `running` and return the fixture plus the agent
/// (whose transcript already holds the provision calls).
async fn provisioned() -> (setu_testkit::TestDb, common::Fx, FakeAgent) {
    let (test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id)
        .await
        .expect("setup provision");
    (test_db, fx, agent)
}

fn tail(transcript: &[String], from: usize) -> Vec<String> {
    transcript[from..].to_vec()
}

#[tokio::test]
async fn update_takes_pre_update_backup_then_switches() {
    let (_test_db, fx, agent) = provisioned().await;
    publish_version(&fx.db, &manifest_toml("0.1.1")).await;
    let before = agent.transcript().len();
    let (ctx, logs) = ctx_with_logs(fx.db.clone(), &agent);

    update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect("update must succeed");

    assert_eq!(
        tail(&agent.transcript(), before),
        [
            "backup",
            "install",
            "stop",
            "switch_version",
            "start",
            "health",
        ]
        .map(str::to_string),
    );

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(site.app_version, "0.1.1");

    let backups = fx.db.backups().list_for_site(&fx.site_id).await.unwrap();
    assert!(
        backups
            .iter()
            .any(|b| b.kind == "pre_update" && b.status == "completed"),
        "backups: {backups:?}"
    );

    let lines = logs.lock().expect("log lock");
    assert!(lines.iter().any(|l| l == "step backup: ok"));
    assert!(lines.iter().any(|l| l == "step switch_version: ok"));
    assert!(lines.iter().any(|l| l == "step finish: ok"));
}

#[tokio::test]
async fn update_rolls_back_when_health_fails() {
    let (_test_db, fx, agent) = provisioned().await;
    publish_version(&fx.db, &manifest_toml("0.1.1")).await;
    agent.fail_next_with("health", "health check failed");
    let before = agent.transcript().len();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect_err("must fail");
    assert!(
        err.message.contains("rollback"),
        "error must mention rollback: {err}"
    );

    // Rolled back: switch to the old version, start, nothing after.
    let tail = tail(&agent.transcript(), before);
    assert_eq!(
        &tail[tail.len() - 2..],
        ["switch_version", "start"].map(str::to_string)
    );
    let switches = agent.switch_versions();
    assert_eq!(switches.as_slice(), ["0.1.1", "0.1.0"]);

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(site.app_version, "0.1.0");
}

#[tokio::test]
async fn bad_checksum_update_never_stops_the_site() {
    let (_test_db, fx, agent) = provisioned().await;
    publish_version(&fx.db, &manifest_toml("0.1.1")).await;
    agent.fail_next_with("install", "checksum mismatch: sha256 does not match");
    let before = agent.transcript().len();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect_err("must fail");
    assert!(err.message.contains("checksum"), "unexpected error: {err}");

    let tail = tail(&agent.transcript(), before);
    assert!(
        !tail.iter().any(|k| k == "stop"),
        "old site must never stop: {tail:?}"
    );

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(site.app_version, "0.1.0");
}

#[tokio::test]
async fn update_command_strategy_runs_command() {
    let (_test_db, fx, agent) = provisioned().await;
    publish_version(&fx.db, &manifest_toml_command("0.2.0")).await;
    let before = agent.transcript().len();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    update::run(&ctx, &fx.site_id, "0.2.0")
        .await
        .expect("command update");

    // The manifest command runs as a Restart + Health (no generic exec
    // command exists in proto); no replace-flow commands are issued.
    let tail = tail(&agent.transcript(), before);
    assert!(tail.iter().any(|k| k == "restart"), "tail: {tail:?}");
    assert!(tail.iter().any(|k| k == "health"), "tail: {tail:?}");
    assert!(!tail.iter().any(|k| k == "install"), "tail: {tail:?}");
    assert!(!tail.iter().any(|k| k == "stop"), "tail: {tail:?}");
    assert!(
        !tail.iter().any(|k| k == "switch_version"),
        "tail: {tail:?}"
    );

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(site.app_version, "0.2.0");
}

#[tokio::test]
async fn update_rollback_switch_back_failure_marks_failed() {
    let (_test_db, fx, agent) = provisioned().await;
    publish_version(&fx.db, &manifest_toml("0.1.1")).await;
    // Health fails so rollback runs; the rollback's switch-back (the
    // second SwitchVersion call) fails too.
    agent.fail_next_with("health", "health check failed");
    agent.fail_with("switch_version", 2, "switch-back failed");
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect_err("must fail");
    assert!(
        err.message.contains("rollback"),
        "error must mention rollback: {err}"
    );

    // The switch-back failed: the site must NOT claim running-on-old.
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.app_version, "0.1.0");
}

#[tokio::test]
async fn update_to_image_is_rejected_before_anything() {
    let (_test_db, fx, agent) = provisioned().await;
    publish_version(&fx.db, &manifest_toml_image("0.3.0")).await;
    let before = agent.transcript().len();
    let backups_before = fx
        .db
        .backups()
        .list_for_site(&fx.site_id)
        .await
        .unwrap()
        .len();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = update::run(&ctx, &fx.site_id, "0.3.0")
        .await
        .expect_err("must fail");
    assert!(
        err.message.contains("image runtime not available"),
        "unexpected error: {err}"
    );
    assert_eq!(agent.transcript().len(), before, "no command may be issued");
    let backups_after = fx
        .db
        .backups()
        .list_for_site(&fx.site_id)
        .await
        .unwrap()
        .len();
    assert_eq!(
        backups_before, backups_after,
        "no pre-update backup may be taken"
    );

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(site.app_version, "0.1.0");
}
