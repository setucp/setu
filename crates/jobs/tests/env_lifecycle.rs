//! `env_update` and the lifecycle jobs over [`FakeAgent`].

mod common;

use std::collections::BTreeMap;

use common::{ctx_with_logs, manifest_toml, setup, stored_env};
use setu_jobs::{env_update, lifecycle, provision, testing::FakeAgent};

fn with_prompt(version: &str) -> String {
    manifest_toml(version).replace(
        "HELLO_PORT = { from = \"port\" }",
        "HELLO_PORT = { from = \"port\" }\nHELLO_LABEL = { value = \"dflt\" }",
    )
}

#[tokio::test]
async fn env_update_merges_changes_writes_env_without_once_and_restarts() {
    let (_test_db, fx) = setup(&with_prompt("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    let before_env = stored_env(&fx.db.sites().get(&fx.site_id).await.unwrap());
    let before = agent.transcript().len();
    logs.lock().unwrap().clear();

    let changes = BTreeMap::from([("HELLO_LABEL".to_string(), "changed".to_string())]);
    env_update::run(&ctx, &fx.site_id, &changes)
        .await
        .expect("env update");

    assert_eq!(
        agent.transcript()[before..],
        ["write_env", "restart"].map(str::to_string)
    );
    let written = agent.write_env_calls().last().unwrap().clone();
    assert!(written.iter().all(|v| !v.once));
    assert!(written
        .iter()
        .any(|v| v.name == "HELLO_LABEL" && v.value == "changed"));
    let after_env = stored_env(&fx.db.sites().get(&fx.site_id).await.unwrap());
    assert_eq!(after_env["HELLO_LABEL"], "changed");
    assert_eq!(after_env["HELLO_SECRET"], before_env["HELLO_SECRET"]);
    assert_eq!(after_env["HELLO_PASSWORD"], before_env["HELLO_PASSWORD"]);
    assert_eq!(
        *logs.lock().unwrap(),
        env_update::STEPS
            .iter()
            .map(|s| format!("step {s}: ok"))
            .collect::<Vec<_>>()
    );

    // Generated variables cannot be set.
    let bad = BTreeMap::from([("HELLO_SECRET".to_string(), "x".to_string())]);
    let err = env_update::run(&ctx, &fx.site_id, &bad)
        .await
        .expect_err("generated var");
    assert!(err.message.contains("generated"), "{err}");
}

#[tokio::test]
async fn lifecycle_stop_start_restart() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    let before = agent.transcript().len();

    lifecycle::run(&ctx, &fx.site_id, lifecycle::Op::Stop)
        .await
        .expect("stop");
    assert_eq!(
        fx.db.sites().get(&fx.site_id).await.unwrap().status,
        "stopped"
    );
    lifecycle::run(&ctx, &fx.site_id, lifecycle::Op::Start)
        .await
        .expect("start");
    lifecycle::run(&ctx, &fx.site_id, lifecycle::Op::Restart)
        .await
        .expect("restart");
    assert_eq!(
        fx.db.sites().get(&fx.site_id).await.unwrap().status,
        "running"
    );
    assert_eq!(
        agent.transcript()[before..],
        ["stop", "start", "restart"].map(str::to_string)
    );

    agent.fail_next("restart");
    lifecycle::run(&ctx, &fx.site_id, lifecycle::Op::Restart)
        .await
        .expect_err("restart fails");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(
        (site.status.as_str(), site.failed_step.as_deref()),
        ("failed", Some("restart"))
    );
}
