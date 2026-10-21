// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
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

    let changes = env_update::EnvChanges {
        manifest: BTreeMap::from([("HELLO_LABEL".to_string(), "changed".to_string())]),
        ..Default::default()
    };
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
    let bad = env_update::EnvChanges {
        manifest: BTreeMap::from([("HELLO_SECRET".to_string(), "x".to_string())]),
        ..Default::default()
    };
    let err = env_update::run(&ctx, &fx.site_id, &bad)
        .await
        .expect_err("generated var");
    assert!(err.message.contains("generated"), "{err}");
}

#[tokio::test]
async fn extra_vars_are_added_masked_by_flag_removed_and_validated() {
    let (_test_db, fx) = setup(&with_prompt("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");

    let add = env_update::EnvChanges {
        extra: BTreeMap::from([
            ("DEMO_MODE".to_string(), ("true".to_string(), false)),
            ("DEMO_TOKEN".to_string(), ("s3cret".to_string(), true)),
        ]),
        ..Default::default()
    };
    env_update::run(&ctx, &fx.site_id, &add).await.expect("add");
    let written = agent.write_env_calls().last().unwrap().clone();
    assert!(written
        .iter()
        .any(|v| v.name == "DEMO_MODE" && v.value == "true"));
    assert!(written
        .iter()
        .any(|v| v.name == "DEMO_TOKEN" && v.value == "s3cret" && v.secret));
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    let raw = site.env.to_string();
    assert!(!raw.contains("s3cret"), "a secret extra is stored sealed");
    assert!(raw.contains("plain:true"), "{raw}");

    let remove = env_update::EnvChanges {
        remove: vec!["DEMO_MODE".to_string()],
        ..Default::default()
    };
    env_update::run(&ctx, &fx.site_id, &remove)
        .await
        .expect("remove");
    let written = agent.write_env_calls().last().unwrap().clone();
    assert!(!written.iter().any(|v| v.name == "DEMO_MODE"));
    assert!(written.iter().any(|v| v.name == "DEMO_TOKEN"));

    for (name, why) in [
        ("PATH", "reserved"),
        ("SETU_X", "reserved"),
        ("lower", "upper-case"),
        ("HELLO_LABEL", "declared by the manifest"),
    ] {
        let bad = env_update::EnvChanges {
            extra: BTreeMap::from([(name.to_string(), ("x".to_string(), false))]),
            ..Default::default()
        };
        let err = env_update::run(&ctx, &fx.site_id, &bad)
            .await
            .expect_err(name);
        assert!(err.message.contains(why), "{name}: {err}");
    }
    let not_extra = env_update::EnvChanges {
        remove: vec!["HELLO_SECRET".to_string()],
        ..Default::default()
    };
    let err = env_update::run(&ctx, &fx.site_id, &not_extra)
        .await
        .expect_err("manifest var");
    assert!(err.message.contains("not an extra"), "{err}");
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
