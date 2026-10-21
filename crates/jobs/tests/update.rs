// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Update state-machine tests over [`FakeAgent`] (plan Task 6).

mod common;

use std::time::Duration;

use common::{
    ctx_with_logs, finish_job, job_ctx, manifest_toml, manifest_toml_image, publish_version, setup,
    stored_env, test_sealer,
};
use setu_jobs::{provision, testing::FakeAgent, update};

/// Provision the fixture to `running` (callers slice the transcript
/// from the length before their own run).
async fn provisioned(src: &str) -> (setu_testkit::TestDb, common::Fx, FakeAgent) {
    let (test_db, fx) = setup(src).await;
    let agent = FakeAgent::new();
    let (ctx, _) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id)
        .await
        .expect("setup provision");
    (test_db, fx, agent)
}

fn tail(agent: &FakeAgent, from: usize) -> Vec<String> {
    agent.transcript()[from..].to_vec()
}

/// [`manifest_toml`] plus an author `value` var, so a merge visibly
/// refreshes it.
fn labeled(version: &str, label: &str) -> String {
    manifest_toml(version).replace(
        "HELLO_PORT = { from = \"port\" }",
        &format!("HELLO_PORT = {{ from = \"port\" }}\nHELLO_LABEL = {{ value = \"{label}\" }}"),
    )
}

/// The agent side of an update (`validate`, `env` and `finish` are
/// control-plane only).
const UPDATE: [&str; 8] = [
    "backup",
    "install",
    "stop",
    "switch_version",
    "write_unit",
    "write_env",
    "start",
    "health",
];

fn update_kinds() -> Vec<String> {
    UPDATE
        .iter()
        .map(std::string::ToString::to_string)
        .collect()
}

#[tokio::test]
async fn update_transcript_is_env_backup_install_stop_switch_unit_env_start_health() {
    let (_test_db, fx, agent) = provisioned(&labeled("0.1.0", "old")).await;
    let before_env = stored_env(&fx.db.sites().get(&fx.site_id).await.unwrap());
    publish_version(&fx.db, &labeled("0.1.1", "new")).await;
    let before = agent.transcript().len();
    let (ctx, job_id, logs) = job_ctx(
        &fx.db,
        &agent,
        "update_site",
        &fx.site_id,
        serde_json::json!({"to_version": "0.1.1"}),
    )
    .await;

    update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect("update must succeed");

    assert_eq!(tail(&agent, before), update_kinds());
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(
        (site.status.as_str(), site.app_version.as_str()),
        ("running", "0.1.1")
    );
    assert_eq!(
        agent.switch_versions().last().map(String::as_str),
        Some("0.1.1")
    );
    let unit = agent.commands_of("write_unit");
    assert_eq!(unit.last().unwrap()["site"]["version"], "0.1.1");

    // The written env is merge(old, V): generated and db values kept,
    // `value` refreshed, `once` never re-emitted.
    let written: std::collections::BTreeMap<String, String> = agent
        .write_env_calls()
        .last()
        .unwrap()
        .iter()
        .map(|v| (v.name.clone(), v.value.clone()))
        .collect();
    assert_eq!(written["HELLO_LABEL"], "new");
    assert_eq!(written["HELLO_SECRET"], before_env["HELLO_SECRET"]);
    assert_eq!(
        written["HELLO_DATABASE_URL"],
        before_env["HELLO_DATABASE_URL"]
    );
    assert!(!written.contains_key("HELLO_PASSWORD"));
    // Storage follows the merge and keeps the `once` value sealed.
    let after_env = stored_env(&site);
    assert_eq!(after_env["HELLO_LABEL"], "new");
    assert_eq!(after_env["HELLO_PASSWORD"], before_env["HELLO_PASSWORD"]);

    let backups = fx.db.backups().list_for_site(&fx.site_id).await.unwrap();
    assert!(backups
        .iter()
        .any(|b| b.kind == "pre_update" && b.status == "completed"));
    let expect: Vec<String> = update::STEPS
        .iter()
        .map(|s| format!("step {s}: ok"))
        .collect();
    assert_eq!(*logs.lock().unwrap(), expect);
    let job = fx.db.jobs().get(&job_id).await.unwrap();
    assert_eq!(job.payload["step"], "finish");
}

#[tokio::test]
async fn update_rolls_back_with_restore_when_health_fails() {
    let (_test_db, fx, agent) = provisioned(&labeled("0.1.0", "old")).await;
    publish_version(&fx.db, &labeled("0.1.1", "new")).await;
    agent.fail_with("health", 2, "health check failed");
    let before = agent.transcript().len();
    let (ctx, job_id, logs) = job_ctx(
        &fx.db,
        &agent,
        "update_site",
        &fx.site_id,
        serde_json::json!({"to_version": "0.1.1"}),
    )
    .await;

    let err = update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect_err("must fail");
    assert!(err.message.starts_with("rollback:"), "{err}");

    let mut expect = update_kinds();
    expect.extend(
        [
            "stop",
            "switch_version",
            "write_unit",
            "write_env",
            "restore",
            "start",
            "health",
        ]
        .map(str::to_string),
    );
    assert_eq!(tail(&agent, before), expect);
    assert_eq!(agent.switch_versions(), ["0.1.0", "0.1.1", "0.1.0"]);
    let units = agent.commands_of("write_unit");
    assert_eq!(units.last().unwrap()["site"]["version"], "0.1.0");
    // The old env, without `once`.
    let restored = agent.write_env_calls().last().unwrap().clone();
    assert!(restored
        .iter()
        .any(|v| v.name == "HELLO_LABEL" && v.value == "old"));
    assert!(restored.iter().all(|v| !v.once));
    // Restore of the pre_update backup, database role and backup dirs.
    let backups = fx.db.backups().list_for_site(&fx.site_id).await.unwrap();
    let pre = backups.iter().find(|b| b.kind == "pre_update").unwrap();
    let restore = &agent.commands_of("restore")[0];
    assert_eq!(restore["backup_id"], pre.id.as_str());
    assert_eq!(restore["dirs"], serde_json::json!(["media"]));

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(
        (site.status.as_str(), site.app_version.as_str()),
        ("running", "0.1.0")
    );
    assert_eq!(stored_env(&site)["HELLO_LABEL"], "old");
    assert!(logs
        .lock()
        .unwrap()
        .iter()
        .any(|l| l == "step rollback: ok"));
    let job = fx.db.jobs().get(&job_id).await.unwrap();
    assert_eq!(job.payload["step"], "rollback");
}

#[tokio::test]
async fn bad_archive_update_never_stops_the_site() {
    let (_test_db, fx, agent) = provisioned(&labeled("0.1.0", "old")).await;
    let before_env = stored_env(&fx.db.sites().get(&fx.site_id).await.unwrap());
    publish_version(&fx.db, &labeled("0.1.1", "new")).await;
    agent.fail_with("install", 2, "archive entry ../x escapes the version dir");
    let before = agent.transcript().len();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect_err("must fail");
    assert!(err.message.contains("escapes"), "{err}");
    assert_eq!(tail(&agent, before), ["backup", "install"]);
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(
        (site.status.as_str(), site.app_version.as_str()),
        ("running", "0.1.0")
    );
    assert_eq!(site.failed_step, None);
    assert_eq!(stored_env(&site), before_env, "stored env put back");
}

#[tokio::test]
async fn update_resumes_from_cursor() {
    let (_test_db, fx, agent) = provisioned(&manifest_toml("0.1.0")).await;
    publish_version(&fx.db, &manifest_toml("0.1.1")).await;
    let before = agent.transcript().len();
    let (ctx, job_id, _) = job_ctx(
        &fx.db,
        &agent,
        "update_site",
        &fx.site_id,
        serde_json::json!({"to_version": "0.1.1"}),
    )
    .await;
    // The control plane dies while `switch_version` is in flight.
    agent.delay("switch_version", Duration::from_secs(30));
    let db = fx.db.clone();
    let site_id = fx.site_id.clone();
    let running = tokio::spawn(async move { update::run(&ctx, &site_id, "0.1.1").await });
    for _ in 0..200 {
        if agent.call_count("switch_version") == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    running.abort();
    let _ = running.await;
    agent.clear_delay("switch_version");
    let job = db.jobs().get(&job_id).await.unwrap();
    assert_eq!(job.payload["step"], "switch_version");
    assert_eq!(
        db.sites().get(&fx.site_id).await.unwrap().status,
        "updating"
    );

    // A new runner picks the job up: it resumes at the cursor.
    let resumed_from = agent.transcript().len();
    let (ctx, _) = ctx_with_logs(fx.db.clone(), &agent);
    let ctx = ctx.with_job(&job_id, common::TEST_WORKER);
    update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect("resumed update succeeds");
    assert_eq!(
        tail(&agent, before)[..4],
        ["backup", "install", "stop", "switch_version"].map(str::to_string)
    );
    assert_eq!(
        tail(&agent, resumed_from),
        [
            "switch_version",
            "write_unit",
            "write_env",
            "start",
            "health"
        ]
        .map(str::to_string)
    );
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(
        (site.status.as_str(), site.app_version.as_str()),
        ("running", "0.1.1")
    );
    finish_job(&fx.db, &job_id, true).await;
}

#[tokio::test]
async fn update_payload_holds_old_env_and_spec_sealed() {
    let (_test_db, fx, agent) = provisioned(&labeled("0.1.0", "old")).await;
    let old_env = stored_env(&fx.db.sites().get(&fx.site_id).await.unwrap());
    publish_version(&fx.db, &labeled("0.1.1", "new")).await;
    let (ctx, job_id, _) = job_ctx(
        &fx.db,
        &agent,
        "update_site",
        &fx.site_id,
        serde_json::json!({"to_version": "0.1.1"}),
    )
    .await;
    update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect("update succeeds");

    let payload = fx.db.jobs().get(&job_id).await.unwrap().payload;
    let raw = payload.to_string();
    let sealer = test_sealer();
    let env = payload["old_env"].as_array().expect("old env array");
    assert_eq!(env.len(), old_env.len());
    for var in env {
        let value = var["value"].as_str().unwrap();
        assert!(value.starts_with("v1:"), "{} not sealed", var["name"]);
        assert_eq!(
            sealer.open(value).unwrap(),
            old_env[var["name"].as_str().unwrap()]
        );
    }
    for plain in old_env.values().filter(|v| v.len() >= 8) {
        assert!(!raw.contains(plain.as_str()), "plaintext env in payload");
    }
    let spec = payload["old_spec"].as_str().expect("old spec");
    assert!(spec.starts_with("v1:"));
    let spec: serde_json::Value = serde_json::from_str(&sealer.open(spec).unwrap()).unwrap();
    assert_eq!(spec["version"], "0.1.0");
    assert_eq!(payload["old_version"], "0.1.0");
}

#[tokio::test]
async fn update_to_image_is_rejected_before_anything() {
    let (_test_db, fx, agent) = provisioned(&manifest_toml("0.1.0")).await;
    publish_version(&fx.db, &manifest_toml_image("0.3.0")).await;
    let before = agent.transcript().len();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = update::run(&ctx, &fx.site_id, "0.3.0")
        .await
        .expect_err("must fail");
    assert!(err.message.contains("image runtime not available"), "{err}");
    assert_eq!(agent.transcript().len(), before, "no command may be issued");
    assert!(fx
        .db
        .backups()
        .list_for_site(&fx.site_id)
        .await
        .unwrap()
        .is_empty());
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(
        (site.status.as_str(), site.app_version.as_str()),
        ("running", "0.1.0")
    );
}

#[tokio::test]
async fn update_rollback_failure_marks_failed() {
    let (_test_db, fx, agent) = provisioned(&manifest_toml("0.1.0")).await;
    publish_version(&fx.db, &manifest_toml("0.1.1")).await;
    agent.fail_with("health", 2, "health check failed");
    // Calls: provision, update, rollback — the switch-back fails.
    agent.fail_with("switch_version", 3, "switch-back failed");
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect_err("must fail");
    assert!(err.message.starts_with("rollback failed"), "{err}");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.failed_step.as_deref(), Some("rollback"));
}

#[tokio::test]
async fn extra_vars_survive_an_update_unless_the_new_version_declares_them() {
    let (_test_db, fx, agent) = provisioned(&labeled("0.1.0", "old")).await;
    let (env_ctx, _) = ctx_with_logs(fx.db.clone(), &agent);
    let extras = setu_jobs::env_update::EnvChanges {
        extra: std::collections::BTreeMap::from([
            ("KEEP_ME".to_string(), ("kept".to_string(), true)),
            ("HELLO_NEXT".to_string(), ("operator".to_string(), false)),
        ]),
        ..Default::default()
    };
    setu_jobs::env_update::run(&env_ctx, &fx.site_id, &extras)
        .await
        .expect("extras");
    let next = labeled("0.1.1", "new").replace(
        "HELLO_PORT = { from = \"port\" }",
        "HELLO_PORT = { from = \"port\" }\nHELLO_NEXT = { value = \"rule\" }",
    );
    publish_version(&fx.db, &next).await;
    let (ctx, _job_id, _logs) = job_ctx(
        &fx.db,
        &agent,
        "update_site",
        &fx.site_id,
        serde_json::json!({"to_version": "0.1.1"}),
    )
    .await;
    update::run(&ctx, &fx.site_id, "0.1.1")
        .await
        .expect("update must succeed");

    let written = agent.write_env_calls().last().unwrap().clone();
    assert!(written
        .iter()
        .any(|v| v.name == "KEEP_ME" && v.value == "kept" && v.secret));
    let next_vars: Vec<_> = written.iter().filter(|v| v.name == "HELLO_NEXT").collect();
    assert_eq!(next_vars.len(), 1);
    assert_eq!(next_vars[0].value, "rule", "the new version's rule wins");
}
