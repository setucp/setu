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
            "write_env",
            "stop",
            "switch_version",
            "start",
            "health",
            // The fixture carries a `once` secret: provision's mirror
            // cleanup rewrites the file without it after first health.
            "write_env",
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
    assert!(lines.iter().any(|l| l == "step write_env: ok"));
    assert!(lines.iter().any(|l| l == "step write_env_cleanup: ok"));
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

/// Fixture twin of [`manifest_toml`] with an author-controlled `value`
/// var (plus the same generated `once` secret): updates must refresh
/// the value while keeping stored secrets bit-for-bit.
fn manifest_toml_labeled(version: &str, label: &str) -> String {
    manifest_toml(version).replace(
        "HELLO_PORT = { from = \"port\" }",
        &format!("HELLO_PORT = {{ from = \"port\" }}\nHELLO_LABEL = {{ value = \"{label}\" }}"),
    )
}

fn write_env_values(agent: &FakeAgent, nth: usize) -> std::collections::BTreeMap<String, String> {
    agent.write_env_calls()[nth]
        .iter()
        .map(|v| (v.name.clone(), v.value.clone()))
        .collect()
}

/// Update refreshes `value` vars from the new manifest but never rotates
/// stored `generate`/`prompt` secrets; the sealed DB copy follows.
#[tokio::test]
async fn update_refreshes_values_but_keeps_stored_secrets() {
    let (test_db, fx) = setup(&manifest_toml_labeled("0.1.0", "label-old")).await;
    let agent = FakeAgent::new();
    let (ctx, _) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id)
        .await
        .expect("setup provision");
    let provisioned_secret = write_env_values(&agent, 0)
        .get("HELLO_SECRET")
        .cloned()
        .expect("provision writes the secret");
    let writes_before = agent.write_env_calls().len();
    publish_version(&fx.db, &manifest_toml_labeled("0.1.1", "label-new")).await;
    let (ctx, _) = ctx_with_logs(fx.db.clone(), &agent);

    update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect("update must succeed");

    // First update-time WriteEnv carries the refreshed value with the
    // untouched secret; the cleanup write drops only `once` vars.
    let vars = write_env_values(&agent, writes_before);
    assert_eq!(
        vars.get("HELLO_LABEL").map(String::as_str),
        Some("label-new")
    );
    assert_eq!(
        vars.get("HELLO_SECRET").map(String::as_str),
        Some(provisioned_secret.as_str()),
        "generated secret must not rotate"
    );
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    let stored: serde_json::Value = site.env.clone();
    let label = stored
        .as_array()
        .expect("env array")
        .iter()
        .find(|v| v["name"] == "HELLO_LABEL")
        .expect("label stored");
    assert_eq!(label["value"], "label-new");
    assert_eq!(label["secret"], false);
    drop(test_db);
}

/// A failed update restores the previous env file before switching back,
/// so the old binary boots with its old values (e.g. a poisoned new
/// default cannot linger).
#[tokio::test]
async fn rollback_restores_previous_env_values() {
    let (test_db, fx) = setup(&manifest_toml_labeled("0.1.0", "label-old")).await;
    let agent = FakeAgent::new();
    let (ctx, _) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id)
        .await
        .expect("setup provision");
    publish_version(&fx.db, &manifest_toml_labeled("0.1.1", "label-new")).await;
    agent.fail_next_with("health", "health check failed");
    let (ctx, _) = ctx_with_logs(fx.db.clone(), &agent);

    update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect_err("must fail");

    // WriteEnv calls: [.., update-merged(label-new), rollback-restore].
    // The restore (last) must carry the old value again.
    let calls = agent.write_env_calls();
    assert!(calls.len() >= 2, "calls: {calls:?}");
    let last: std::collections::BTreeMap<String, String> = calls
        .last()
        .expect("restore write")
        .iter()
        .map(|v| (v.name.clone(), v.value.clone()))
        .collect();
    assert_eq!(
        last.get("HELLO_LABEL").map(String::as_str),
        Some("label-old"),
        "rollback must restore previous env"
    );
    drop(test_db);
}
